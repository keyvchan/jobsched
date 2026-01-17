use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug)]
pub enum Request {
    Run {
        command: String,
        args: Vec<String>,
    },
    Status {
        job_id: u32,
    },
}

#[derive(Serialize, Deserialize, Debug)]
pub enum Response {
    Success {
        message: String,
    },
    Status {
        status: String,
        stdout: Option<String>,
        stderr: Option<String>,
    },
    Error {
        message: String,
    },
}
