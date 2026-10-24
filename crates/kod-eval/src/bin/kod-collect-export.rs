use clap::Parser;
#[derive(Parser)]
struct Args {
    #[arg(long)]
    repo: std::path::PathBuf,
    #[arg(long)]
    out: std::path::PathBuf,
    #[arg(long)]
    base: String,
}
fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    kod_eval::export::export_index(&args.repo, &args.out, &args.base)
}
