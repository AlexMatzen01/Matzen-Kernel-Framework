use spin::Lazy;
use x86_64::structures::idt::{InterruptDescriptorTable, InterruptStackFrame};

static IDT: Lazy<InterruptDescriptorTable> = Lazy::new(|| {
    let mut idt = InterruptDescriptorTable::new();
    idt.divide_error.set_handler_fn(divide_by_zero_handler);
    idt.page_fault.set_handler_fn(page_fault_handler);
    idt.double_fault.set_handler_fn(double_fault_handler);
    idt.breakpoint.set_handler_fn(breakpoint_handler);
    idt
});

pub fn init_idt() {
    IDT.load();
    log::info!("IDT loaded");
}

pub fn enable() {
    x86_64::instructions::interrupts::enable();
}

extern "x86-interrupt" fn divide_by_zero_handler(stack_frame: InterruptStackFrame) {
    log::error!("EXCEPTION: DIVIDE BY ZERO\n{:#?}", stack_frame);
    crate::core::runtime::idle_loop();
}

extern "x86-interrupt" fn breakpoint_handler(stack_frame: InterruptStackFrame) {
    log::warn!("EXCEPTION: BREAKPOINT\n{:#?}", stack_frame);
}

extern "x86-interrupt" fn double_fault_handler(
    stack_frame: InterruptStackFrame,
    _error_code: u64,
) -> ! {
    log::error!("EXCEPTION: DOUBLE FAULT\n{:#?}", stack_frame);
    crate::core::runtime::halt();
}

extern "x86-interrupt" fn page_fault_handler(
    stack_frame: InterruptStackFrame,
    error_code: x86_64::structures::idt::PageFaultErrorCode,
) {
    use x86_64::registers::control::Cr2;

    let fault_addr = Cr2::read();
    log::error!(
        "EXCEPTION: PAGE FAULT at {:?} (error: {:#x})\n{:#?}",
        fault_addr,
        error_code,
        stack_frame
    );
    crate::core::runtime::halt();
}
