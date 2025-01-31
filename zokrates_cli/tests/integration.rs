extern crate assert_cli;
extern crate serde_json;

#[cfg(test)]
mod integration {
    use assert_cli;
    use bincode::{deserialize_from, Infinite};
    use std::fs;
    use std::fs::File;
    use std::io::{BufReader, Read};
    use std::panic;
    use std::path::Path;
    use tempdir::TempDir;
    use zokrates_abi::{parse_strict, Encode};
    use zokrates_core::ir;
    use zokrates_field::field::FieldPrime;

    #[test]
    #[ignore]
    fn test_compile_and_witness_dir() {
        // install nodejs dependencies for the verification contract tester
        install_nodejs_deps();

        let dir = Path::new("./tests/code");
        assert!(dir.is_dir());
        for entry in fs::read_dir(dir).unwrap() {
            let entry = entry.unwrap();
            let path = entry.path();
            if path.extension().unwrap() == "witness" {
                let program_name =
                    Path::new(Path::new(path.file_stem().unwrap()).file_stem().unwrap());
                let prog = dir.join(program_name).with_extension("zok");
                let witness = dir.join(program_name).with_extension("expected.witness");
                let json_input = dir.join(program_name).with_extension("arguments.json");
                test_compile_and_witness(
                    program_name.to_str().unwrap(),
                    &prog,
                    &json_input,
                    &witness,
                );
            }
        }
    }

    fn install_nodejs_deps() {
        let out_dir = concat!(env!("OUT_DIR"), "/contract");

        assert_cli::Assert::command(&["npm", "install"])
            .current_dir(out_dir)
            .succeeds()
            .unwrap();
    }

    fn test_compile_and_witness(
        program_name: &str,
        program_path: &Path,
        inputs_path: &Path,
        expected_witness_path: &Path,
    ) {
        let tmp_dir = TempDir::new(".tmp").unwrap();
        let tmp_base = tmp_dir.path();
        let test_case_path = tmp_base.join(program_name);
        let flattened_path = tmp_base.join(program_name).join("out");
        let witness_path = tmp_base.join(program_name).join("witness");
        let inline_witness_path = tmp_base.join(program_name).join("inline_witness");
        let proof_path = tmp_base.join(program_name).join("proof.json");
        let verification_key_path = tmp_base
            .join(program_name)
            .join("verification")
            .with_extension("key");
        let proving_key_path = tmp_base
            .join(program_name)
            .join("proving")
            .with_extension("key");
        let verification_contract_path = tmp_base
            .join(program_name)
            .join("verifier")
            .with_extension("sol");

        // create a tmp folder to store artifacts
        fs::create_dir(test_case_path).unwrap();

        // prepare compile arguments
        let compile = vec![
            "../target/release/zokrates",
            "compile",
            "-i",
            program_path.to_str().unwrap(),
            "-o",
            flattened_path.to_str().unwrap(),
            "--light",
        ];

        // compile
        assert_cli::Assert::command(&compile).succeeds().unwrap();

        // COMPUTE_WITNESS

        // derive program signature from IR program representation
        let file = File::open(&flattened_path)
            .map_err(|why| format!("couldn't open {}: {}", flattened_path.display(), why))
            .unwrap();

        let mut reader = BufReader::new(file);

        let ir_prog: ir::Prog<FieldPrime> = deserialize_from(&mut reader, Infinite)
            .map_err(|why| why.to_string())
            .unwrap();

        let signature = ir_prog.signature.clone();

        // run witness-computation for ABI-encoded inputs through stdin
        let json_input_str = fs::read_to_string(inputs_path).unwrap();

        let compute = vec![
            "../target/release/zokrates",
            "compute-witness",
            "-i",
            flattened_path.to_str().unwrap(),
            "-o",
            witness_path.to_str().unwrap(),
            "--stdin",
            "--abi",
        ];

        assert_cli::Assert::command(&compute)
            .stdin(&json_input_str)
            .succeeds()
            .unwrap();

        // run witness-computation for raw-encoded inputs (converted) with `-a <arguments>`
        let inputs_abi: zokrates_abi::Inputs<zokrates_field::field::FieldPrime> =
            parse_strict(&json_input_str, signature.inputs)
                .map(|parsed| zokrates_abi::Inputs::Abi(parsed))
                .map_err(|why| why.to_string())
                .unwrap();
        let inputs_raw: Vec<_> = inputs_abi
            .encode()
            .into_iter()
            .map(|v| v.to_string())
            .collect();

        let mut compute_inline = vec![
            "../target/release/zokrates",
            "compute-witness",
            "-i",
            flattened_path.to_str().unwrap(),
            "-o",
            inline_witness_path.to_str().unwrap(),
            "-a",
        ];

        for arg in &inputs_raw {
            compute_inline.push(arg);
        }

        assert_cli::Assert::command(&compute_inline)
            .succeeds()
            .unwrap();

        // load the expected witness
        let mut expected_witness_file = File::open(&expected_witness_path).unwrap();
        let mut expected_witness = String::new();
        expected_witness_file
            .read_to_string(&mut expected_witness)
            .unwrap();

        // load the actual witness
        let mut witness_file = File::open(&witness_path).unwrap();
        let mut witness = String::new();
        witness_file.read_to_string(&mut witness).unwrap();

        // load the actual inline witness
        let mut inline_witness_file = File::open(&inline_witness_path).unwrap();
        let mut inline_witness = String::new();
        inline_witness_file
            .read_to_string(&mut inline_witness)
            .unwrap();

        assert_eq!(inline_witness, witness);

        for line in expected_witness.as_str().split("\n") {
            assert!(
                witness.contains(line),
                "Witness generation failed for {}\n\nLine \"{}\" not found in witness",
                program_path.to_str().unwrap(),
                line
            );
        }

        #[cfg(feature = "libsnark")]
        let schemes = ["pghr13", "gm17", "g16"];
        #[cfg(not(feature = "libsnark"))]
        let schemes = ["g16"];

        for scheme in &schemes {
            // SETUP
            assert_cli::Assert::command(&[
                "../target/release/zokrates",
                "setup",
                "-i",
                flattened_path.to_str().unwrap(),
                "-p",
                proving_key_path.to_str().unwrap(),
                "-v",
                verification_key_path.to_str().unwrap(),
                "--proving-scheme",
                scheme,
            ])
            .succeeds()
            .unwrap();

            // EXPORT-VERIFIER
            assert_cli::Assert::command(&[
                "../target/release/zokrates",
                "export-verifier",
                "-i",
                verification_key_path.to_str().unwrap(),
                "-o",
                verification_contract_path.to_str().unwrap(),
                "--proving-scheme",
                scheme,
            ])
            .succeeds()
            .unwrap();

            // EXPORT-VERIFIER (AMV)
            assert_cli::Assert::command(&[
                "../target/release/zokrates",
                "export-verifier-avm",
                "-i",
                verification_key_path.to_str().unwrap(),
                "-o",
                verification_contract_path.to_str().unwrap(),
                "--proving-scheme",
                scheme,
            ])
                .succeeds()
                .unwrap();

            // GENERATE-PROOF
            assert_cli::Assert::command(&[
                "../target/release/zokrates",
                "generate-proof",
                "-i",
                flattened_path.to_str().unwrap(),
                "-w",
                witness_path.to_str().unwrap(),
                "-p",
                proving_key_path.to_str().unwrap(),
                "--proving-scheme",
                scheme,
                "-j",
                proof_path.to_str().unwrap(),
            ])
            .succeeds()
            .unwrap();

            // TEST VERIFIER

            assert_cli::Assert::command(&[
                "node",
                "test.js",
                verification_contract_path.to_str().unwrap(),
                proof_path.to_str().unwrap(),
                scheme,
                "v1",
            ])
            .current_dir(concat!(env!("OUT_DIR"), "/contract"))
            .succeeds()
            .unwrap();
        }
    }
}
