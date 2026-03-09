use std::io::BufRead;

pub(crate) fn read_stdin_line() -> String {
    let stdin = std::io::stdin();
    let mut line = String::new();
    let _ = stdin.lock().read_line(&mut line);
    line
}
