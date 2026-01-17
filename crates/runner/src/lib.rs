use tokio::process::{Command, Child};
use tokio::io;
use std::process::Stdio;

/// Spawns a process with the given program and arguments, capturing its stdout and stderr.
///
/// # Arguments
///
/// * `program` - The program to execute.
/// * `args` - The arguments to pass to the program.
///
/// # Returns
///
/// A `Result` containing the `Child` handle of the spawned process (with stdout/stderr piped),
/// or an `io::Error` if the process fails to spawn.
pub fn spawn_process(program: &str, args: &[String]) -> io::Result<Child> {
    Command::new(program)
        .args(args)
        .stdout(Stdio::piped()) // Capture stdout
        .stderr(Stdio::piped()) // Capture stderr
        .spawn()
}

/// Runs a process with the given program and arguments, waiting for it to complete.
/// This function now uses tokio's Command and Child.
///
/// # Arguments
///
/// * `program` - The program to execute.
/// * `args` - The arguments to pass to the program.
///
/// # Returns
///
/// A `Result` containing the `ExitStatus` of the process, or an `io::Error` if
/// the process fails to spawn or run.
pub async fn run_process(program: &str, args: &[String]) -> io::Result<std::process::ExitStatus> {
    let mut child = Command::new(program)
        .args(args)
        .spawn()?;
    Ok(child.wait().await?)
}


#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_spawn_process_success() {
        let child_result = spawn_process("echo", &["test".to_string()]);
        assert!(child_result.is_ok());
        let mut child = child_result.unwrap();
        assert!(child.wait().await.unwrap().success());
    }

    #[test]
    fn test_spawn_process_command_not_found() {
        let child_result = spawn_process("non_existent_command_xyz", &[]);
        assert!(child_result.is_err());
    }

    #[tokio::test]
    async fn test_run_process_success() {
        // A command that is likely to exist on most systems and exit successfully.
        let result = run_process("echo", &["hello".to_string()]).await;
        assert!(result.is_ok());
        assert!(result.unwrap().success());
    }

    #[tokio::test]
    async fn test_run_process_command_not_found() {
        // A command that is unlikely to exist.
        let result = run_process("command_that_does_not_exist_12345", &[]).await;
        assert!(result.is_err());
    }
}
