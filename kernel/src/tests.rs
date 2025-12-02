#![cfg(test)]

pub fn run(tests: &[&dyn Fn()]) {
    for test in tests {
        test();
    }
}
