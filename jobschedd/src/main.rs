use jobsched_common::{Request, Response};

use std::collections::HashMap;
use std::os::unix::process::ExitStatusExt;

use std::sync::{Arc, Mutex};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};
use tracing::{error, info}; // Added for status.signal()

const SOCKET_PATH: &str = "/tmp/jobsched.sock";

#[derive(Debug, Clone)]
enum JobStatus {
    Waiting {
        pid: u32,
    }, // New status for when we are waiting for output
    Finished {
        exit_status_code: i32,
        stdout: String,
        stderr: String,
    },
    Signaled {
        signal_code: i32,
        stdout: String,
        stderr: String,
    },
    FailedToSpawn {
        error: String,
    },
}

#[derive(Debug, Clone)]
struct Job {
    id: u32,
    command: String,
    args: Vec<String>,
    status: JobStatus,
}

type JobStore = Arc<Mutex<HashMap<u32, Job>>>;
type JobIdCounter = Arc<Mutex<u32>>;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt::init();

    if let Err(e) = std::fs::remove_file(SOCKET_PATH) {
        if e.kind() != std::io::ErrorKind::NotFound {
            error!("Failed to remove existing socket file: {}", e);
            return Err(e.into());
        }
    }

    let listener = UnixListener::bind(SOCKET_PATH)?;
    info!("Daemon listening on {}", SOCKET_PATH);

    let job_store: JobStore = Arc::new(Mutex::new(HashMap::new()));
    let job_id_counter: JobIdCounter = Arc::new(Mutex::new(0));

    loop {
        match listener.accept().await {
            Ok((stream, _)) => {
                info!("Accepted new client");
                let job_store_clone = job_store.clone();
                let job_id_counter_clone = job_id_counter.clone();
                tokio::spawn(async move {
                    handle_client(stream, job_store_clone, job_id_counter_clone).await;
                });
            }
            Err(e) => {
                error!("Failed to accept connection: {}", e);
            }
        }
    }
}

async fn handle_client(mut stream: UnixStream, job_store: JobStore, job_id_counter: JobIdCounter) {
    // This loop will now handle multiple requests over the same connection
    loop {
        // Read the 4-byte length prefix
        let mut len_bytes = [0u8; 4];
        if stream.read_exact(&mut len_bytes).await.is_err() {
            // Client disconnected or error, break the loop
            info!("Client disconnected or failed to read length prefix.");
            break;
        }
        let len = u32::from_le_bytes(len_bytes) as usize;

        let mut buffer = vec![0u8; len];
        if stream.read_exact(&mut buffer).await.is_err() {
            error!("Client disconnected or failed to read message body.");
            break;
        }

        // Deserialize request
        let request: Request = match serde_json::from_slice(&buffer) {
            Ok(req) => req,
            Err(e) => {
                error!("Failed to deserialize request: {}", e);
                let response = Response::Error {
                    message: format!("Failed to deserialize request: {}", e),
                };
                let response_bytes = serde_json::to_vec(&response).unwrap();
                let response_len = response_bytes.len() as u32;
                if stream.write_all(&response_len.to_le_bytes()).await.is_err() {
                    error!("Failed to write response length for error to stream.");
                    break;
                }
                if stream.write_all(&response_bytes).await.is_err() {
                    error!("Failed to write error response body to stream.");
                    break;
                }
                continue; // Continue to next potential request
            }
        };

        info!("Received request: {:?}", request);

        match request {
            Request::Run { command, args } => {
                let job_id = {
                    let mut counter = job_id_counter.lock().unwrap();
                    *counter += 1;
                    *counter
                };

                // Clone command and args for the blocking spawn_process call
                let command_for_spawn = command.clone();
                let args_for_spawn = args.clone();

                let spawn_result = tokio::task::spawn_blocking(move || {
                    runner::spawn_process(&command_for_spawn, &args_for_spawn)
                })
                .await
                .unwrap(); // Unwrap the JoinHandle result

                match spawn_result {
                    Ok(mut child) => {
                        let pid = child.id().expect("Child process did not have a PID");
                        let new_job = Job {
                            id: job_id,
                            command: command.clone(),
                            args: args.clone(),
                            status: JobStatus::Waiting { pid }, // Use Waiting status initially
                        };
                        job_store.lock().unwrap().insert(job_id, new_job);
                        info!(
                            "Job {} started (PID: {}) for command: {} {:?}",
                            job_id, pid, command, args
                        );

                        // Send initial success response
                        let response = Response::Success {
                            message: format!("Job {} started (PID: {}).", job_id, pid),
                        };
                        send_response(&mut stream, &response).await;

                        let mut stdout = child.stdout.take().expect("Child stdout not captured");
                        let mut stderr = child.stderr.take().expect("Child stderr not captured");

                        let stdout_task = tokio::spawn(async move {
                            let mut output = String::new();
                            stdout
                                .read_to_string(&mut output)
                                .await
                                .unwrap_or_else(|e| {
                                    error!("Failed to read stdout for job {}: {}", pid, e);
                                    0
                                });
                            output
                        });

                        let stderr_task = tokio::spawn(async move {
                            let mut output = String::new();
                            stderr
                                .read_to_string(&mut output)
                                .await
                                .unwrap_or_else(|e| {
                                    error!("Failed to read stderr for job {}: {}", pid, e);
                                    0
                                });
                            output
                        });

                        let (child_wait_result, stdout_output, stderr_output) =
                            tokio::join!(child.wait(), stdout_task, stderr_task);

                        let status_result = match child_wait_result {
                            Ok(exit_status) => exit_status,
                            Err(e) => {
                                error!("Error waiting for job {} (PID: {}): {}", job_id, pid, e);
                                {
                                    let mut store = job_store.lock().unwrap();
                                    if let Some(job) = store.get_mut(&job_id) {
                                        job.status = JobStatus::FailedToSpawn {
                                            error: format!("Failed to wait: {}", e),
                                        };
                                    }
                                }
                                let response = Response::Error {
                                    message: format!("Failed to wait: {}", e),
                                };
                                send_response(&mut stream, &response).await;
                                continue;
                            }
                        };

                        let stdout_str =
                            stdout_output.expect("Failed to get stdout output task result");
                        let stderr_str =
                            stderr_output.expect("Failed to get stderr output task result");

                        let final_status = if let Some(code) = status_result.code() {
                            JobStatus::Finished {
                                exit_status_code: code,
                                stdout: stdout_str.clone(),
                                stderr: stderr_str.clone(),
                            }
                        } else {
                            JobStatus::Signaled {
                                signal_code: status_result.signal().unwrap_or(-1),
                                stdout: stdout_str.clone(),
                                stderr: stderr_str.clone(),
                            }
                        };

                        {
                            let mut store = job_store.lock().unwrap();
                            if let Some(job) = store.get_mut(&job_id) {
                                job.status = final_status.clone();
                                info!(
                                    "Job {} (PID: {}) finished with status: {:?}",
                                    job_id, pid, job.status
                                );
                            }
                        }

                        // Send final status response
                        let (status_str, stdout_content, stderr_content) = match final_status {
                            JobStatus::Finished {
                                exit_status_code,
                                stdout,
                                stderr,
                            } => (
                                format!("Finished (Exit Code: {})", exit_status_code),
                                Some(stdout),
                                Some(stderr),
                            ),
                            JobStatus::Signaled {
                                signal_code,
                                stdout,
                                stderr,
                            } => (
                                format!("Terminated by signal (Code: {})", signal_code),
                                Some(stdout),
                                Some(stderr),
                            ),
                            _ => ("Unknown status".to_string(), None, None),
                        };

                        let response = Response::Status {
                            status: format!(
                                "Job {}: {} {} -> {}",
                                job_id,
                                command,
                                args.join(" "),
                                status_str
                            ),
                            stdout: stdout_content,
                            stderr: stderr_content,
                        };
                        send_response(&mut stream, &response).await;
                    }
                    Err(e) => {
                        let new_job = Job {
                            id: job_id,
                            command: command.clone(),
                            args: args.clone(),
                            status: JobStatus::FailedToSpawn {
                                error: e.to_string(),
                            },
                        };
                        job_store.lock().unwrap().insert(job_id, new_job);
                        error!("Failed to spawn job {}: {}", job_id, e);
                        let response = Response::Error {
                            message: format!("Failed to run process: {}", e),
                        };
                        send_response(&mut stream, &response).await;
                    }
                }
            }
            Request::Status { job_id } => {
                let response = {
                    let store = job_store.lock().unwrap();
                    if let Some(job) = store.get(&job_id) {
                        let (status_str, stdout_content, stderr_content) = match &job.status {
                            JobStatus::Waiting { pid } => {
                                (format!("Waiting for output (PID: {})", pid), None, None)
                            }
                            JobStatus::Finished {
                                exit_status_code,
                                stdout,
                                stderr,
                            } => (
                                format!("Finished (Exit Code: {})", exit_status_code),
                                Some(stdout.clone()),
                                Some(stderr.clone()),
                            ),
                            JobStatus::Signaled {
                                signal_code,
                                stdout,
                                stderr,
                            } => (
                                format!("Terminated by signal (Code: {})", signal_code),
                                Some(stdout.clone()),
                                Some(stderr.clone()),
                            ),
                            JobStatus::FailedToSpawn { error } => {
                                (format!("Failed to spawn: {}", error), None, None)
                            }
                        };
                        Response::Status {
                            status: format!(
                                "Job {}: {} {} -> {}",
                                job.id,
                                job.command,
                                job.args.join(" "),
                                status_str
                            ),
                            stdout: stdout_content,
                            stderr: stderr_content,
                        }
                    } else {
                        Response::Error {
                            message: format!("Job {} not found.", job_id),
                        }
                    }
                };
                send_response(&mut stream, &response).await;
            }
        };
    } // End of loop
}

async fn send_response(stream: &mut UnixStream, response: &Response) {
    let response_bytes = serde_json::to_vec(response).unwrap();
    let response_len = response_bytes.len() as u32;
    if stream.write_all(&response_len.to_le_bytes()).await.is_err() {
        error!("Failed to write response length to stream.");
    }
    if stream.write_all(&response_bytes).await.is_err() {
        error!("Failed to write response body to stream.");
    }
}
