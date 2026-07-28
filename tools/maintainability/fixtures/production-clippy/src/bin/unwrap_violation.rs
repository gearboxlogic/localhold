fn unwrap_value(value: Option<u8>) -> u8 {
    value.unwrap()
}

fn main() {
    let _value = unwrap_value(Some(1_u8));
}
