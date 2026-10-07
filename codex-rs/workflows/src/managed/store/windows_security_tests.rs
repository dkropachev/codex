use super::*;

#[test]
fn rejects_existing_permissive_directory() {
    let root = tempfile::tempdir().expect("root");
    let path = root.path().join("permissive");
    let sddl = "D:P(A;OICI;FA;;;WD)"
        .encode_utf16()
        .chain(Some(0))
        .collect::<Vec<_>>();
    let mut descriptor = ptr::null_mut();
    assert_ne!(
        unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                sddl.as_ptr(),
                /*stringsdrevision*/ 1,
                &mut descriptor,
                ptr::null_mut(),
            )
        },
        0
    );
    let attributes = SECURITY_ATTRIBUTES {
        nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: descriptor,
        bInheritHandle: 0,
    };
    let wide = path
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect::<Vec<_>>();
    let created = unsafe { CreateDirectoryW(wide.as_ptr(), &attributes) };
    unsafe { LocalFree(descriptor as _) };
    assert_ne!(created, 0, "{}", io::Error::last_os_error());
    assert!(open_directory(&path, /*private*/ true, /*desired_access*/ 0).is_err());
}

#[test]
fn rejects_directory_junctions() {
    let root = tempfile::tempdir().expect("root");
    let target = root.path().join("target");
    let junction = root.path().join("junction");
    std::fs::create_dir(&target).expect("target directory");
    let output = std::process::Command::new("cmd")
        .args(["/C", "mklink", "/J"])
        .arg(&junction)
        .arg(&target)
        .output()
        .expect("create junction");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(open_directory(&junction, /*private*/ false, /*desired_access*/ 0).is_err());
    assert!(!is_symbolic_link_reparse_point(&junction).expect("junction reparse tag"));
    std::fs::remove_dir(&junction).expect("remove junction");
}

#[test]
fn creates_and_reopens_a_private_non_reparse_directory() {
    let root = tempfile::tempdir().expect("root");
    let path = root.path().join("managed");
    let (first, created) = create_private_directory(&path).expect("create private directory");
    assert!(created);
    let (second, created) = create_private_directory(&path).expect("reopen private directory");
    assert!(!created);
    let third = open_directory(&path, /*private*/ true, /*desired_access*/ 0)
        .expect("validate private directory");
    drop((first, second, third));

    let private_file = path.join("private.json");
    let created_file = create_private_file(&private_file).expect("create private file");
    open_private_file(&private_file).expect("reopen private file");
    drop(created_file);
    let inherited_file = path.join("inherited.json");
    std::fs::write(&inherited_file, b"inherited").expect("write inherited file");
    assert!(open_private_file(&inherited_file).is_err());
}
