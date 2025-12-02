use x86_64::instructions::hlt;

pub fn health_check() {
    log::info!("Runtime health checks passed (placeholder)");
}

pub fn idle_loop() -> ! {
    loop {
        hlt();
    }
}

pub fn halt() -> ! {
    loop {
        hlt();
    }
}
