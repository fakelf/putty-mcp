pub mod process;
pub mod protocol;
pub mod security;
pub mod terminal;
pub const OUTPUT_LIMIT: usize = 64 * 1024;
pub const RING_SIZE: usize = 2 * 1024 * 1024;
pub const COMMANDS: [&str; 5] = ["uname -a", "uptime", "df -h", "free -m", "iostat -x 1 5"];
