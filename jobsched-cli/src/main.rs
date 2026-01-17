use clap::{Parser, Subcommand};
use jobsched_common::{Request, Response};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;

const SOCKET_PATH: &str = "/tmp/jobsched.sock";

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,

    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    command_and_args: Vec<String>,
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// Run a new job
    #[command(trailing_var_arg = true, allow_hyphen_values = true)]
    Run { command_and_args: Vec<String> },
    /// Get the status of a job
    Status { job_id: u32 },
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();

    let request = match (cli.command, cli.command_and_args) {
        (
            Some(Commands::Run {
                mut command_and_args,
            }),
            _,
        ) => {
            if command_and_args.is_empty() {
                eprintln!("Error: No command provided to 'run'");
                return Ok(());
            }
            Request::Run {
                command: command_and_args.remove(0),
                args: command_and_args,
            }
        }
        (Some(Commands::Status { job_id }), _) => Request::Status { job_id },
        (None, mut args) => {
            if args.is_empty() {
                eprintln!(
                    "Error: No command provided. Usage: jobsched-cli run <command> or jobsched-cli -- <command>"
                );
                return Ok(());
            }
            Request::Run {
                command: args.remove(0),
                args,
            }
        }
    };

    println!("request: {:?}", request);

    let mut stream = match UnixStream::connect(SOCKET_PATH).await {
        Ok(stream) => stream,
        Err(e) => {
            eprintln!("Failed to connect to daemon: {}. Is the daemon running?", e);
            return Err(e.into());
        }
    };

    let request_bytes = serde_json::to_vec(&request)?;
    let request_len = request_bytes.len() as u32;
    stream.write_all(&request_len.to_le_bytes()).await?;
    stream.write_all(&request_bytes).await?;

    loop {
        let mut response_len_bytes = [0u8; 4];
        match stream.read_exact(&mut response_len_bytes).await {
            Ok(_) => (),
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(e) => return Err(e.into()),
        }
        let response_len = u32::from_le_bytes(response_len_bytes) as usize;

        let mut response_bytes = vec![0u8; response_len];
        stream.read_exact(&mut response_bytes).await?;

        let response: Response = serde_json::from_slice(&response_bytes)?;

        match response {
            Response::Success { message } => {
                println!("Success: {}", message);
            }
            Response::Status {
                status,
                stdout,
                stderr,
            } => {
                println!("{}", status);
                if let Some(out) = stdout {
                    if !out.is_empty() {
                        println!("\n--- STDOUT ---");
                        println!("{}", out);
                    }
                }
                if let Some(err) = stderr {
                    if !err.is_empty() {
                        println!("\n--- STDERR ---");
                        println!("{}", err);
                    }
                }
                break;
            }
            Response::Error { message } => {
                eprintln!("Error: {}", message);
                break;
            }
        }
    }

    Ok(())
}
