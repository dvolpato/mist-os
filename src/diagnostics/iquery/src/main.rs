// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::Error;
use fuchsia_async as fasync;
use iquery::command_line::CommandLine;
use iquery::commands::{ArchiveAccessorProvider, Command};

#[cfg(test)]
#[macro_use]
mod tests;

#[fasync::run_singlethreaded]
async fn main() -> Result<(), Error> {
    let command_line: CommandLine = argh::from_env();
    let provider = ArchiveAccessorProvider;
    match command_line.execute(&provider).await {
        Ok(result) => {
            println!("{}", result);
        }
        Err(err) => {
            eprintln!("{}", err);
            std::process::exit(1);
        }
    }
    Ok(())
}
