fn main() {
    if std::env::args().nth(1).as_deref() == Some("--production") {
        std::process::exit(aivs_core::production::cli::main(
            std::env::args().skip(2).collect(),
        ));
    }
    if let Err(error) = aivs_core::serve(
        std::io::BufReader::new(std::io::stdin()),
        std::io::stdout().lock(),
    ) {
        eprintln!("AIVS engine: {error}");
        std::process::exit(1);
    }
}
