#[test]
fn reports_libprojectm_4_2() {
    assert_eq!(projectmp4::ffi::version().0, 4);
    assert_eq!(projectmp4::ffi::version().1, 2);
}

#[test]
fn master_only_symbols_are_linked() {
    assert!(projectmp4::ffi::master_symbols_present());
}
