use std::io::BufRead;

pub(crate) fn read_stdin_line() -> String {
    let stdin = std::io::stdin();
    let mut line = String::new();
    if let Err(e) = stdin.lock().read_line(&mut line) {
        eprintln!("warning: failed to read stdin: {}", e);
    }
    line
}
