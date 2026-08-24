//! Runtime capability view generated from the Desktop API IDL.

pub fn available(native_desktop: bool) -> Vec<String> {
    let mut result = super::idl_generated::ALWAYS
        .iter()
        .map(|v| (*v).to_owned())
        .collect::<Vec<_>>();
    if native_desktop {
        result.extend(
            super::idl_generated::NATIVE_DESKTOP
                .iter()
                .map(|v| (*v).to_owned()),
        );
    }
    result
}

pub fn experimental() -> Vec<String> {
    super::idl_generated::EXPERIMENTAL
        .iter()
        .map(|v| (*v).to_owned())
        .collect()
}
