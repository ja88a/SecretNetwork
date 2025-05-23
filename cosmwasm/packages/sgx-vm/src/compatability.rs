use parity_wasm::elements::{deserialize_buffer, External, ImportEntry, Module};
use std::collections::BTreeSet;
use std::collections::HashSet;
use std::iter::FromIterator;

use crate::errors::{VmError, VmResult};
use crate::features::required_features_from_module;

/// Lists all v0.10 imports we provide upon instantiating the instance in Instance::from_module()
/// This should be updated when new imports are added
const SUPPORTED_IMPORTS_V010: &[&str] = &[
    "env.db_read",
    "env.db_write",
    "env.db_remove",
    "env.canonicalize_address",
    "env.humanize_address",
    "env.query_chain",
    "env.secp256k1_verify",
    "env.secp256k1_recover_pubkey",
    "env.secp256k1_sign",
    "env.ed25519_verify",
    "env.ed25519_batch_verify",
    "env.ed25519_sign",
    #[cfg(feature = "iterator")]
    "env.db_scan",
    #[cfg(feature = "iterator")]
    "env.db_next",
    #[cfg(feature = "debug-print")]
    "env.debug_print",
];

/// Lists all v1 imports we provide upon instantiating the instance in Instance::from_module()
/// This should be updated when new imports are added
const SUPPORTED_IMPORTS_V1: &[&str] = &[
    "env.db_read",
    "env.db_write",
    "env.db_remove",
    "env.addr_validate",
    "env.addr_canonicalize",
    "env.addr_humanize",
    "env.secp256k1_verify",
    "env.secp256k1_recover_pubkey",
    "env.secp256k1_sign",
    "env.ed25519_verify",
    "env.ed25519_batch_verify",
    "env.ed25519_sign",
    "env.dcap_quote_verify",
    "env.debug",
    "env.query_chain",
    #[cfg(feature = "iterator")]
    "env.db_scan",
    #[cfg(feature = "iterator")]
    "env.db_next",
    "env.gas_evaporate",
    "env.check_gas"
];

/// Lists all entry points we expect to be present when calling a v0.10 contract.
/// Basically, anything that is used in calls.rs
/// This is unlikely to change much, must be frozen at 1.0 to avoid breaking existing contracts
const REQUIRED_EXPORTS_V010: &[&str] = &[
    "cosmwasm_vm_version_3",
    "query",
    "init",
    "handle",
    "allocate",
    "deallocate",
];

/// Lists all entry points we expect to be present when calling a v1 contract.
/// Basically, anything that is used in calls.rs
/// This is unlikely to change much, must be frozen at 1.0 to avoid breaking existing contracts
const REQUIRED_EXPORTS_V1: &[&str] = &[
    "interface_version_8",
    // IO
    "allocate",
    "deallocate",
    // Required entry points
    "instantiate",
];

pub const REQUIRED_IBC_EXPORTS: &[&str] = &[
    "ibc_channel_open",
    "ibc_channel_connect",
    "ibc_channel_close",
    "ibc_packet_receive",
    "ibc_packet_ack",
    "ibc_packet_timeout",
];

const MEMORY_LIMIT: u32 = 512; // in pages

/// Checks if the data is valid wasm and compatibility with the CosmWasm API (imports and exports)
pub fn check_wasm(wasm_code: &[u8], supported_features: &HashSet<String>) -> VmResult<()> {
    let module = match deserialize_buffer(&wasm_code) {
        Ok(deserialized) => deserialized,
        Err(err) => {
            return Err(VmError::static_validation_err(format!(
                "Wasm bytecode could not be deserialized. Deserialization error: \"{}\"",
                err
            )));
        }
    };
    check_wasm_memories(&module)?;
    check_wasm_features(&module, supported_features)?;

    let check_v010_exports_result = check_wasm_exports(&module, REQUIRED_EXPORTS_V010);
    let check_v010_imports_result = check_wasm_imports(&module, SUPPORTED_IMPORTS_V010);
    let is_v010 = check_v010_exports_result.is_ok() && check_v010_imports_result.is_ok();

    let check_v1_exports_result = check_wasm_exports(&module, REQUIRED_EXPORTS_V1);
    let check_v1_imports_result = check_wasm_imports(&module, SUPPORTED_IMPORTS_V1);
    let is_v1 = check_v1_exports_result.is_ok() && check_v1_imports_result.is_ok();

    if !is_v010 && !is_v1 {
        let errors = vec![
            check_v010_exports_result,
            check_v010_imports_result,
            check_v1_exports_result,
            check_v1_imports_result,
        ];

        return Err(VmError::static_validation_err(format!("Contract is not CosmWasm v0.10 or v1. To support v0.10 please fix the former two errors, to supports v1 please fix the latter two errors: ${:?}", errors)));
    }

    Ok(())
}

fn check_wasm_memories(module: &Module) -> VmResult<()> {
    let section = match module.memory_section() {
        Some(section) => section,
        None => {
            return Err(VmError::static_validation_err(
                "Wasm contract doesn't have a memory section",
            ));
        }
    };

    let memories = section.entries();
    if memories.len() != 1 {
        return Err(VmError::static_validation_err(
            "Wasm contract must contain exactly one memory",
        ));
    }

    let memory = memories[0];
    // println!("Memory: {:?}", memory);
    let limits = memory.limits();

    if limits.initial() > MEMORY_LIMIT {
        return Err(VmError::static_validation_err(format!(
            "Wasm contract memory's minimum must not exceed {} pages.",
            MEMORY_LIMIT
        )));
    }

    if limits.maximum() != None {
        return Err(VmError::static_validation_err(
            "Wasm contract memory's maximum must be unset. The host will set it for you.",
        ));
    }
    Ok(())
}

pub fn check_wasm_exports(module: &Module, required_exports: &[&str]) -> VmResult<()> {
    let available_exports: Vec<String> = module.export_section().map_or(vec![], |export_section| {
        export_section
            .entries()
            .iter()
            .map(|entry| entry.field().to_string())
            .collect()
    });

    for required_export in required_exports {
        if !available_exports.iter().any(|x| x == required_export) {
            return Err(VmError::static_validation_err(format!(
                "Wasm contract doesn't have required export: \"{}\". Exports required by VM: {:?}.",
                required_export, required_exports
            )));
        }
    }
    Ok(())
}

/// Checks if the import requirements of the contract are satisfied.
/// When this is not the case, we either have an incompatibility between contract and VM
/// or a error in the contract.
fn check_wasm_imports(module: &Module, supported_imports: &[&str]) -> VmResult<()> {
    let required_imports: Vec<ImportEntry> = module
        .import_section()
        .map_or(vec![], |import_section| import_section.entries().to_vec());
    for required_import in required_imports {
        let full_name = format!("{}.{}", required_import.module(), required_import.field());
        if !supported_imports.contains(&full_name.as_str()) {
            return Err(VmError::static_validation_err(format!(
                "Wasm contract requires unsupported import: \"{}\". Imports supported by VM: {:?}.",
                full_name, supported_imports
            )));
        }

        match required_import.external() {
            External::Function(_) => {}, // ok
            _ => return Err(VmError::static_validation_err(format!(
                "Wasm contract requires non-function import: \"{}\". Right now, all supported imports are functions.",
                full_name
            ))),
        };
    }

    Ok(())
}

fn check_wasm_features(module: &Module, supported_features: &HashSet<String>) -> VmResult<()> {
    let required_features = required_features_from_module(module);
    if !required_features.is_subset(supported_features) {
        // We switch to BTreeSet to get a sorted error message
        let unsupported = BTreeSet::from_iter(required_features.difference(&supported_features));
        return Err(VmError::static_validation_err(format!(
            "Wasm contract requires unsupported features: {:?}",
            unsupported
        )));
    }
    Ok(())
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::errors::VmError;
    use std::iter::FromIterator;
    use wabt::wat2wasm;

    static CONTRACT_0_6: &[u8] = include_bytes!("../testdata/contract_0.6.wasm");
    static CONTRACT_0_7: &[u8] = include_bytes!("../testdata/contract_0.7.wasm");
    static CONTRACT: &[u8] = include_bytes!("../testdata/contract.wasm");
    static CORRUPTED: &[u8] = include_bytes!("../testdata/corrupted.wasm");

    fn default_features() -> HashSet<String> {
        HashSet::from_iter(["staking".to_string()].iter().cloned())
    }

    #[test]
    fn test_check_wasm() {
        // this is our reference check, must pass
        check_wasm(CONTRACT, &default_features()).unwrap();
    }

    #[test]
    fn test_check_wasm_old_contract() {
        match check_wasm(CONTRACT_0_7, &default_features()) {
            Err(VmError::StaticValidationErr { msg, .. }) => assert!(msg.starts_with(
                "Wasm contract doesn't have required export: \"cosmwasm_vm_version_3\""
            )),
            Err(e) => panic!("Unexpected error {:?}", e),
            Ok(_) => panic!("This must not succeeed"),
        };

        match check_wasm(CONTRACT_0_6, &default_features()) {
            Err(VmError::StaticValidationErr { msg, .. }) => assert!(msg.starts_with(
                "Wasm contract doesn't have required export: \"cosmwasm_vm_version_3\""
            )),
            Err(e) => panic!("Unexpected error {:?}", e),
            Ok(_) => panic!("This must not succeeed"),
        };
    }

    #[test]
    fn test_check_wasm_corrupted_data() {
        match check_wasm(CORRUPTED, &default_features()) {
            Err(VmError::StaticValidationErr { msg, .. }) => {
                assert!(msg.starts_with("Wasm bytecode could not be deserialized."))
            }
            Err(e) => panic!("Unexpected error {:?}", e),
            Ok(_) => panic!("This must not succeeed"),
        }
    }

    #[test]
    fn test_check_wasm_memories_ok() {
        let wasm = wat2wasm("(module (memory 1))").unwrap();
        check_wasm_memories(&deserialize_buffer(&wasm).unwrap()).unwrap()
    }

    #[test]
    fn test_check_wasm_memories_no_memory() {
        let wasm = wat2wasm("(module)").unwrap();
        match check_wasm_memories(&deserialize_buffer(&wasm).unwrap()) {
            Err(VmError::StaticValidationErr { msg, .. }) => {
                assert!(msg.starts_with("Wasm contract doesn't have a memory section"));
            }
            Err(e) => panic!("Unexpected error {:?}", e),
            Ok(_) => panic!("Didn't reject wasm with invalid api"),
        }
    }

    #[test]
    fn test_check_wasm_memories_two_memories() {
        // Generated manually because wat2wasm protects us from creating such Wasm:
        // "error: only one memory block allowed"
        let wasm = hex::decode(concat!(
            "0061736d", // magic bytes
            "01000000", // binary version (uint32)
            "05",       // section type (memory)
            "05",       // section length
            "02",       // number of memories
            "0009",     // element of type "resizable_limits", min=9, max=unset
            "0009",     // element of type "resizable_limits", min=9, max=unset
        ))
        .unwrap();

        match check_wasm_memories(&deserialize_buffer(&wasm).unwrap()) {
            Err(VmError::StaticValidationErr { msg, .. }) => {
                assert!(msg.starts_with("Wasm contract must contain exactly one memory"));
            }
            Err(e) => panic!("Unexpected error {:?}", e),
            Ok(_) => panic!("Didn't reject wasm with invalid api"),
        }
    }

    #[test]
    fn test_check_wasm_memories_zero_memories() {
        // Generated manually because wat2wasm would not create an empty memory section
        let wasm = hex::decode(concat!(
            "0061736d", // magic bytes
            "01000000", // binary version (uint32)
            "05",       // section type (memory)
            "01",       // section length
            "00",       // number of memories
        ))
        .unwrap();

        match check_wasm_memories(&deserialize_buffer(&wasm).unwrap()) {
            Err(VmError::StaticValidationErr { msg, .. }) => {
                assert!(msg.starts_with("Wasm contract must contain exactly one memory"));
            }
            Err(e) => panic!("Unexpected error {:?}", e),
            Ok(_) => panic!("Didn't reject wasm with invalid api"),
        }
    }

    #[test]
    fn test_check_wasm_memories_initial_size() {
        let wasm_ok = wat2wasm("(module (memory 512))").unwrap();
        check_wasm_memories(&deserialize_buffer(&wasm_ok).unwrap()).unwrap();

        let wasm_too_big = wat2wasm("(module (memory 513))").unwrap();
        match check_wasm_memories(&deserialize_buffer(&wasm_too_big).unwrap()) {
            Err(VmError::StaticValidationErr { msg, .. }) => {
                assert!(msg.starts_with("Wasm contract memory's minimum must not exceed 512 pages"));
            }
            Err(e) => panic!("Unexpected error {:?}", e),
            Ok(_) => panic!("Didn't reject wasm with invalid api"),
        }
    }

    #[test]
    fn test_check_wasm_memories_maximum_size() {
        let wasm_max = wat2wasm("(module (memory 1 5))").unwrap();
        match check_wasm_memories(&deserialize_buffer(&wasm_max).unwrap()) {
            Err(VmError::StaticValidationErr { msg, .. }) => {
                assert!(msg.starts_with("Wasm contract memory's maximum must be unset"));
            }
            Err(e) => panic!("Unexpected error {:?}", e),
            Ok(_) => panic!("Didn't reject wasm with invalid api"),
        }
    }

    #[test]
    fn test_check_wasm_exports() {
        // this is invalid, as it doesn't contain all required exports
        const WAT_MISSING_EXPORTS: &'static str = r#"
            (module
              (type $t0 (func (param i32) (result i32)))
              (func $add_one (export "add_one") (type $t0) (param $p0 i32) (result i32)
                get_local $p0
                i32.const 1
                i32.add))
        "#;
        let wasm_missing_exports = wat2wasm(WAT_MISSING_EXPORTS).unwrap();

        let module = deserialize_buffer(&wasm_missing_exports).unwrap();
        match check_wasm_exports(&module, REQUIRED_EXPORTS_V010) {
            Err(VmError::StaticValidationErr { msg, .. }) => {
                assert!(msg.starts_with(
                    "Wasm contract doesn't have required export: \"cosmwasm_vm_version_3\""
                ));
            }
            Err(e) => panic!("Unexpected error {:?}", e),
            Ok(_) => panic!("Didn't reject wasm with invalid api"),
        }
    }

    #[test]
    fn test_check_wasm_exports_of_old_contract() {
        let module = deserialize_buffer(CONTRACT_0_7).unwrap();
        match check_wasm_exports(&module, REQUIRED_EXPORTS_V010) {
            Err(VmError::StaticValidationErr { msg, .. }) => {
                assert!(msg.starts_with(
                    "Wasm contract doesn't have required export: \"cosmwasm_vm_version_3\""
                ));
            }
            Err(e) => panic!("Unexpected error {:?}", e),
            Ok(_) => panic!("Didn't reject wasm with invalid api"),
        }
    }

    #[test]
    fn check_wasm_imports_ok() {
        let wasm = wat2wasm(
            r#"(module
            (import "env" "db_read" (func (param i32 i32) (result i32)))
            (import "env" "db_write" (func (param i32 i32) (result i32)))
            (import "env" "db_remove" (func (param i32) (result i32)))
            (import "env" "canonicalize_address" (func (param i32 i32) (result i32)))
            (import "env" "humanize_address" (func (param i32 i32) (result i32)))
        )"#,
        )
        .unwrap();
        check_wasm_imports(&deserialize_buffer(&wasm).unwrap(), SUPPORTED_IMPORTS_V010).unwrap();
    }

    #[test]
    fn test_check_wasm_imports_of_old_contract() {
        let module = deserialize_buffer(CONTRACT_0_7).unwrap();
        match check_wasm_imports(&module, SUPPORTED_IMPORTS_V010) {
            Err(VmError::StaticValidationErr { msg, .. }) => {
                assert!(
                    msg.starts_with("Wasm contract requires unsupported import: \"env.db_read\"")
                );
            }
            Err(e) => panic!("Unexpected error {:?}", e),
            Ok(_) => panic!("Didn't reject wasm with invalid api"),
        }
    }

    #[test]
    fn test_check_wasm_imports_wrong_type() {
        let wasm = wat2wasm(r#"(module (import "env" "db_read" (memory 1 1)))"#).unwrap();
        match check_wasm_imports(&deserialize_buffer(&wasm).unwrap(), SUPPORTED_IMPORTS_V010) {
            Err(VmError::StaticValidationErr { msg, .. }) => {
                assert!(
                    msg.starts_with("Wasm contract requires non-function import: \"env.db_read\"")
                );
            }
            Err(e) => panic!("Unexpected error {:?}", e),
            Ok(_) => panic!("Didn't reject wasm with invalid api"),
        }
    }

    #[test]
    fn check_wasm_features_ok() {
        let wasm = wat2wasm(
            r#"(module
            (type (func))
            (func (type 0) nop)
            (export "requires_water" (func 0))
            (export "requires_" (func 0))
            (export "requires_nutrients" (func 0))
            (export "require_milk" (func 0))
            (export "REQUIRES_air" (func 0))
            (export "requires_sun" (func 0))
        )"#,
        )
        .unwrap();
        let module = deserialize_buffer(&wasm).unwrap();
        let supported = HashSet::from_iter(
            [
                "water".to_string(),
                "nutrients".to_string(),
                "sun".to_string(),
                "freedom".to_string(),
            ]
            .iter()
            .cloned(),
        );
        check_wasm_features(&module, &supported).unwrap();
    }

    #[test]
    fn check_wasm_features_fails_for_missing() {
        let wasm = wat2wasm(
            r#"(module
            (type (func))
            (func (type 0) nop)
            (export "requires_water" (func 0))
            (export "requires_" (func 0))
            (export "requires_nutrients" (func 0))
            (export "require_milk" (func 0))
            (export "REQUIRES_air" (func 0))
            (export "requires_sun" (func 0))
        )"#,
        )
        .unwrap();
        let module = deserialize_buffer(&wasm).unwrap();

        // Support set 1
        let supported = HashSet::from_iter(
            [
                "water".to_string(),
                "nutrients".to_string(),
                "freedom".to_string(),
            ]
            .iter()
            .cloned(),
        );
        match check_wasm_features(&module, &supported).unwrap_err() {
            VmError::StaticValidationErr { msg, .. } => assert_eq!(
                msg,
                "Wasm contract requires unsupported features: {\"sun\"}"
            ),
            _ => panic!("Got unexpected error"),
        }

        // Support set 2
        let supported = HashSet::from_iter(
            [
                "nutrients".to_string(),
                "freedom".to_string(),
                "Water".to_string(), // features are case sensitive (and lowercase by convention)
            ]
            .iter()
            .cloned(),
        );
        match check_wasm_features(&module, &supported).unwrap_err() {
            VmError::StaticValidationErr { msg, .. } => assert_eq!(
                msg,
                "Wasm contract requires unsupported features: {\"sun\", \"water\"}"
            ),
            _ => panic!("Got unexpected error"),
        }

        // Support set 3
        let supported = HashSet::from_iter(["freedom".to_string()].iter().cloned());
        match check_wasm_features(&module, &supported).unwrap_err() {
            VmError::StaticValidationErr { msg, .. } => assert_eq!(
                msg,
                "Wasm contract requires unsupported features: {\"nutrients\", \"sun\", \"water\"}"
            ),
            _ => panic!("Got unexpected error"),
        }

        // Support set 4
        let supported = HashSet::from_iter([].iter().cloned());
        match check_wasm_features(&module, &supported).unwrap_err() {
            VmError::StaticValidationErr { msg, .. } => assert_eq!(
                msg,
                "Wasm contract requires unsupported features: {\"nutrients\", \"sun\", \"water\"}"
            ),
            _ => panic!("Got unexpected error"),
        }
    }
}
