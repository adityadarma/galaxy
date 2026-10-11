#![no_main]

use galaxy_core::lsp::parse_json_rpc_payload;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|input: &[u8]| {
    let _ = parse_json_rpc_payload(input);
});
