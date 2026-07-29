#[cfg(not(debug_assertions))]
fn unwrap_value(value: Option<u8>) -> u8 {
    value.unwrap()
}

#[cfg(not(debug_assertions))]
fn main() {
    let _value = unwrap_value(Some(1_u8));
}

#[cfg(debug_assertions)]
fn main() {}
