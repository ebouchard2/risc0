// Copyright 2023 RISC Zero, Inc.
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

use clap::{Parser, Subcommand};
use risc0_zkvm::{prove::default_prover, ExecutorEnv};
use risc0_zkvm_fault::{FAULT_CHECKER_ELF, FAULT_CHECKER_ID};
use risc0_zkvm_methods::{FIB_ELF, FIB_ID};
use which::which;
use xshell::{cmd, Shell};

#[derive(Parser)]
struct Cli {
    #[command(subcommand)]
    cmd: Commands,
}

#[derive(Subcommand)]
enum Commands {
    Install,
    GenReceipt,
    BootstrapFault,
}

impl Commands {
    fn run(&self) {
        match self {
            Commands::Install => self.cmd_install(),
            Commands::GenReceipt => self.cmd_gen_receipt(),
            Commands::BootstrapFault => self.cmd_bootstrap_fault_checker(),
        }
    }

    fn cmd_install(&self) {
        install_wasm_tools();
    }

    fn cmd_gen_receipt(&self) {
        let iterations = 100;
        let env = ExecutorEnv::builder()
            .add_input(&[iterations])
            .build()
            .unwrap();
        let receipt = default_prover().prove_elf(env, FIB_ELF).unwrap();
        let receipt_bytes = bincode::serialize(&receipt).unwrap();

        let rust_code = format!(
            r##"
pub const FIB_ID: [u32; 8] = {FIB_ID:?};
pub const FIB_RECEIPT: &[u8] = &{receipt_bytes:?};
"##
        );

        std::fs::write("risc0/zkvm/receipts/src/receipts.rs", rust_code).unwrap();
    }

    fn cmd_bootstrap_fault_checker(&self) {
        let rust_code = format!(
            r##"// Copyright 2023 RISC Zero, Inc.
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

// Warning: this code is generated by cargo xtask and should not be modified by
// hand.

/// The image ID of the fault checker guest program
#[rustfmt::skip]
pub const FAULT_CHECKER_ID: [u32; 8] = {FAULT_CHECKER_ID:?};

/// The ELF of the fault checker guest program
#[rustfmt::skip]
pub const FAULT_CHECKER_ELF: &[u8] = &{FAULT_CHECKER_ELF:?};
"##
        );

        std::fs::write("risc0/zkvm/src/fault_ids.rs", rust_code).unwrap();
    }
}

fn install_wasm_tools() {
    if which("wask-pack").is_err() {
        let sh = Shell::new().unwrap();
        cmd!(sh, "cargo install --locked wasm-pack").run().unwrap();
    }
}

fn main() {
    Cli::parse().cmd.run();
}
