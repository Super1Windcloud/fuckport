use std::collections::BTreeMap;
use std::mem::MaybeUninit;
use std::path::{Path, PathBuf};

use core_foundation::base::{CFType, TCFType};
use core_foundation::bundle::{CFBundle, CFBundleGetValueForInfoDictionaryKey};
use core_foundation::string::CFString;
use core_foundation::url::CFURL;
use sysinfo::Pid;

pub(super) fn memory_bytes(pid: Pid, resident_bytes: u64) -> u64 {
    physical_footprint(pid).unwrap_or(resident_bytes)
}

fn physical_footprint(pid: Pid) -> Option<u64> {
    let pid = libc::pid_t::try_from(pid.as_u32()).ok()?;
    let mut usage = MaybeUninit::<libc::rusage_info_v2>::uninit();
    // SAFETY: RUSAGE_INFO_V2 writes a complete rusage_info_v2 into this buffer.
    // The API takes a pointer to the buffer cast as *mut rusage_info_t, not a
    // pointer to a separate pointer. Read the initialized buffer only on success.
    let result =
        unsafe { libc::proc_pid_rusage(pid, libc::RUSAGE_INFO_V2, usage.as_mut_ptr().cast()) };
    if result != 0 {
        return None;
    }
    // SAFETY: The successful call above initialized the entire V2 structure.
    Some(unsafe { usage.assume_init() }.ri_phys_footprint)
}

#[derive(Default)]
pub(super) struct AppNames {
    bundle_names: BTreeMap<PathBuf, String>,
}

impl AppNames {
    pub(super) fn for_executable(&mut self, executable: &Path) -> Option<String> {
        let bundle_path = application_bundle(executable)?;
        Some(
            self.bundle_names
                .entry(bundle_path.to_path_buf())
                .or_insert_with(|| {
                    bundle_display_name(bundle_path).unwrap_or_else(|| {
                        bundle_path
                            .file_stem()
                            .unwrap_or_default()
                            .to_string_lossy()
                            .into_owned()
                    })
                })
                .clone(),
        )
    }
}

fn application_bundle(executable: &Path) -> Option<&Path> {
    // Use the owning application rather than a nested Helper.app or embedded runtime.
    executable
        .ancestors()
        .skip(1)
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("app"))
        })
        .last()
}

fn bundle_display_name(path: &Path) -> Option<String> {
    let bundle = CFBundle::new(CFURL::from_path(path, true)?)?;
    ["CFBundleDisplayName", "CFBundleName"]
        .into_iter()
        .find_map(|key| bundle_string(&bundle, key))
}

fn bundle_string(bundle: &CFBundle, key: &str) -> Option<String> {
    let key = CFString::new(key);
    // SAFETY: Both references remain alive for the call. The nullable, borrowed result
    // is checked and retained under the Get rule before its type is inspected.
    let value = unsafe {
        let value = CFBundleGetValueForInfoDictionaryKey(
            bundle.as_concrete_TypeRef(),
            key.as_concrete_TypeRef(),
        );
        if value.is_null() {
            return None;
        }
        CFType::wrap_under_get_rule(value)
    };
    let value = value.downcast::<CFString>()?.to_string();
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_string())
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    #[test]
    fn memory_reads_physical_footprint_for_a_running_process() {
        let pid = Pid::from_u32(std::process::id());
        assert!(physical_footprint(pid).expect("current process should be accessible") > 0);
        let bytes = memory_bytes(pid, u64::MAX);
        assert!(bytes > 0);
        assert_ne!(
            bytes,
            u64::MAX,
            "native footprint should replace the RSS fallback"
        );
    }

    #[test]
    fn memory_falls_back_when_process_is_unavailable() {
        let pid = Pid::from_u32(i32::MAX as u32);
        assert_eq!(physical_footprint(pid), None);
        assert_eq!(memory_bytes(pid, 123_456), 123_456);
    }

    #[test]
    fn memory_rejects_pids_outside_native_range() {
        let pid = Pid::from_u32(u32::MAX);
        assert_eq!(physical_footprint(pid), None);
        assert_eq!(memory_bytes(pid, 654_321), 654_321);
    }

    struct TestBundle(PathBuf);

    impl TestBundle {
        fn new(info: &str) -> Self {
            static NEXT_ID: AtomicUsize = AtomicUsize::new(0);
            let path = std::env::temp_dir().join(format!(
                "fuckport-app-name-{}-{}.app",
                std::process::id(),
                NEXT_ID.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir_all(path.join("Contents/MacOS")).unwrap();
            fs::write(
                path.join("Contents/Info.plist"),
                format!(
                    "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
                     <!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \
                     \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
                     <plist version=\"1.0\"><dict>{info}</dict></plist>"
                ),
            )
            .unwrap();
            Self(path)
        }

        fn executable(&self) -> PathBuf {
            self.0.join("Contents/MacOS/internal-process")
        }

        fn name(&self) -> String {
            self.0.file_stem().unwrap().to_string_lossy().into_owned()
        }
    }

    impl Drop for TestBundle {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.0).unwrap();
        }
    }

    #[test]
    fn helpers_and_embedded_runtimes_belong_to_outer_application() {
        for executable in [
            "/Applications/Example IDE.app/Contents/MacOS/launcher",
            "/Applications/Example IDE.app/Contents/jbr/Contents/Home/bin/java",
            "/Applications/Example IDE.app/Contents/Frameworks/Helper.app/Contents/MacOS/helper",
            "/private/var/folders/random/AppTranslocation/id/d/Example IDE.app/Contents/MacOS/launcher",
        ] {
            assert_eq!(
                application_bundle(Path::new(executable))
                    .unwrap()
                    .file_name()
                    .unwrap(),
                "Example IDE.app"
            );
        }
    }

    #[test]
    fn plain_executables_and_app_like_directory_names_are_not_bundles() {
        for executable in [
            "/usr/bin/python3",
            "/opt/example.application/bin/node",
            "/opt/my.app.backup/bin/node",
            "/usr/bin/tool.app",
        ] {
            assert!(application_bundle(Path::new(executable)).is_none());
            assert!(
                AppNames::default()
                    .for_executable(Path::new(executable))
                    .is_none()
            );
        }
    }

    #[test]
    fn display_name_takes_priority_over_bundle_and_executable_names() {
        let bundle = TestBundle::new(
            "<key>CFBundleDisplayName</key><string>  示例 App  </string>\
             <key>CFBundleName</key><string>InternalName</string>",
        );
        assert_eq!(
            AppNames::default().for_executable(&bundle.executable()),
            Some("示例 App".to_string())
        );
    }

    #[test]
    fn localized_display_name_takes_priority_over_global_name() {
        let bundle = TestBundle::new(
            "<key>CFBundleDisplayName</key><string>Global Name</string>\
             <key>CFBundleDevelopmentRegion</key><string>en</string>",
        );
        let resources = bundle.0.join("Contents/Resources/en.lproj");
        fs::create_dir_all(&resources).unwrap();
        fs::write(
            resources.join("InfoPlist.strings"),
            "\"CFBundleDisplayName\" = \"Localized Name\";",
        )
        .unwrap();
        assert_eq!(
            AppNames::default().for_executable(&bundle.executable()),
            Some("Localized Name".to_string())
        );
    }

    #[test]
    fn binary_plist_display_name_is_supported() {
        use core_foundation::dictionary::CFDictionary;
        use core_foundation::propertylist::{
            CFPropertyListSubClass, create_data, kCFPropertyListBinaryFormat_v1_0,
        };

        let bundle = TestBundle::new("");
        let info = CFDictionary::from_CFType_pairs(&[(
            CFString::new("CFBundleDisplayName"),
            CFString::new("Binary App"),
        )])
        .to_untyped()
        .to_CFPropertyList();
        let data =
            create_data(info.as_concrete_TypeRef(), kCFPropertyListBinaryFormat_v1_0).unwrap();
        fs::write(bundle.0.join("Contents/Info.plist"), data.bytes()).unwrap();
        assert_eq!(
            AppNames::default().for_executable(&bundle.executable()),
            Some("Binary App".to_string())
        );
    }

    #[test]
    fn invalid_or_empty_display_name_falls_back_to_bundle_name() {
        for value in ["<string> \n </string>", "<integer>42</integer>"] {
            let bundle = TestBundle::new(&format!(
                "<key>CFBundleDisplayName</key>{value}\
                 <key>CFBundleName</key><string>Friendly Name</string>"
            ));
            assert_eq!(
                AppNames::default().for_executable(&bundle.executable()),
                Some("Friendly Name".to_string())
            );
        }
    }

    #[test]
    fn absent_or_unreadable_metadata_falls_back_to_app_directory_name() {
        for info in [
            "",
            "<key>CFBundleName</key><string> </string>",
            "<key>CFBundleName</key><array/>",
        ] {
            let bundle = TestBundle::new(info);
            assert_eq!(
                AppNames::default().for_executable(&bundle.executable()),
                Some(bundle.name())
            );
        }
        let bundle = TestBundle::new("");
        fs::write(bundle.0.join("Contents/Info.plist"), b"invalid plist").unwrap();
        assert_eq!(
            AppNames::default().for_executable(&bundle.executable()),
            Some(bundle.name())
        );
        let bundle = TestBundle::new("");
        fs::remove_file(bundle.0.join("Contents/Info.plist")).unwrap();
        assert_eq!(
            AppNames::default().for_executable(&bundle.executable()),
            Some(bundle.name())
        );
    }

    #[test]
    fn processes_in_same_application_share_a_cached_name() {
        let bundle = TestBundle::new("<key>CFBundleName</key><string>Owner App</string>");
        let mut names = AppNames::default();
        assert_eq!(
            names.for_executable(&bundle.executable()),
            Some("Owner App".to_string())
        );
        assert_eq!(
            names.for_executable(
                &bundle
                    .0
                    .join("Contents/Frameworks/Helper.app/Contents/MacOS/helper")
            ),
            Some("Owner App".to_string())
        );
        assert_eq!(names.bundle_names.len(), 1);
    }

    #[test]
    fn catalog_uses_app_name_and_preserves_running_process_name() {
        use std::process::{Child, Command};

        struct TestProcess(Child);
        impl Drop for TestProcess {
            fn drop(&mut self) {
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }

        let bundle = TestBundle::new("<key>CFBundleName</key><string>Example Editor</string>");
        fs::copy("/bin/sleep", bundle.executable()).unwrap();
        let mut catalog = super::super::ProcessCatalog::load().unwrap();
        let child = TestProcess(Command::new(bundle.executable()).arg("30").spawn().unwrap());
        catalog.refresh_interactive();
        let record = catalog
            .process_records()
            .into_iter()
            .find(|record| record.pid.as_u32() == child.0.id())
            .expect("test process should be present in the catalog");
        assert_eq!(record.app_name, "Example Editor");
        assert_eq!(record.name, "internal-process");
        let footprint = physical_footprint(record.pid).unwrap();
        assert_eq!(record.memory_bytes, footprint);
        assert_eq!(
            catalog.initial_start_time(record.pid),
            Some(record.start_time)
        );

        drop(child);
        catalog.refresh_interactive();
        assert!(catalog.system().process(record.pid).is_none());
        assert_eq!(catalog.initial_start_time(record.pid), None);
    }
}
