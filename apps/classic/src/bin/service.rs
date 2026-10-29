//! neochumby-classic-service: what classic keeps doing while another app
//! is in front (svc.rs). The session runs it, and restarts it if it exits.

fn main() {
    #[cfg(target_arch = "arm")]
    std::process::exit(neochumby_classic::svc::device::run());
    #[cfg(not(target_arch = "arm"))]
    {
        eprintln!("neochumby-classic-service needs a Chumby");
        std::process::exit(2);
    }
}
