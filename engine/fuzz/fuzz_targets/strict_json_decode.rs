#![no_main]

#[path = "../../../xtask/src/strict_json.rs"]
mod strict_json;

use libfuzzer_sys::fuzz_target;
use serde::Deserialize;
use serde_json::Value;

fuzz_target!(|data: &[u8]| {
    if let Ok(value) = strict_json::decode::<Value>(data) {
        let mut decoder = serde_json::Deserializer::from_slice(data);
        let ordinary = Value::deserialize(&mut decoder).expect("strict JSON is valid JSON");
        decoder
            .end()
            .expect("strict JSON consumes the complete input");
        assert_eq!(value, ordinary);
        let encoded = serde_json::to_vec(&value).expect("JSON values serialize");
        assert_eq!(
            strict_json::decode::<Value>(&encoded).expect("canonical JSON decodes"),
            value
        );
        let mut duplicated = br#"{"entry":"#.to_vec();
        duplicated.extend_from_slice(&encoded);
        duplicated.extend_from_slice(br#","entry":"#);
        duplicated.extend_from_slice(&encoded);
        duplicated.push(b'}');
        if serde_json::from_slice::<Value>(&duplicated).is_ok() {
            assert!(strict_json::decode::<Value>(&duplicated).is_err());
        }
    }
});
