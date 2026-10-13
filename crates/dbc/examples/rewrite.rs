//! Parse a DBC and write it back (for checking output against other tools).
fn main() {
    let path = std::env::args().nth(1).unwrap();
    let (d, _, enc) = dbc::read_file(path.as_ref()).unwrap();
    use std::io::Write;
    std::io::stdout().write_all(&dbc::encoding::encode(&dbc::write(&d), enc)).unwrap();
}
