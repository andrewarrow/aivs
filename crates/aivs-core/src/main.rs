fn main() {
    if let Err(error) = aivs_core::serve(
        std::io::BufReader::new(std::io::stdin()),
        std::io::stdout().lock(),
    ) {
        eprintln!("AIVS engine: {error}");
        std::process::exit(1);
    }
}
