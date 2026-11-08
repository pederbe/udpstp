//! WinGet manifests for the immutable Windows release executable.
use sha2::{Digest, Sha256};
use std::{fs, path::Path};

pub fn run(args: &[String]) -> Result<(), String> {
    let [version, executable, output] = args else {
        return Err("usage: cargo xtask winget VERSION EXECUTABLE OUTPUT".into());
    };
    if version.split('.').count() != 3
        || !version
            .split('.')
            .all(|s| !s.is_empty() && s.chars().all(|c| c.is_ascii_digit()))
    {
        return Err("expected a stable version".into());
    }
    let bytes = fs::read(executable).map_err(|e| e.to_string())?;
    if !bytes.starts_with(b"MZ") {
        return Err("expected a Windows executable".into());
    }
    let hash: String = Sha256::digest(&bytes)
        .iter()
        .map(|b| format!("{b:02X}"))
        .collect();
    let directory = Path::new(output)
        .join("manifests/p/pederbe/udpstp")
        .join(version);
    fs::create_dir_all(&directory).map_err(|e| e.to_string())?;
    let identity = format!("PackageIdentifier: pederbe.udpstp\nPackageVersion: {version}\n");
    let files = [
        (
            "version",
            "pederbe.udpstp.yaml",
            "DefaultLocale: en-US\n".to_owned(),
        ),
        (
            "installer",
            "pederbe.udpstp.installer.yaml",
            format!(
                "InstallerType: portable\nCommands:\n- udpstp\nInstallers:\n- Architecture: x64\n  InstallerUrl: https://github.com/pederbe/udpstp/releases/download/v{version}/udpstp.exe\n  InstallerSha256: {hash}\n"
            ),
        ),
        (
            "defaultLocale",
            "pederbe.udpstp.locale.en-US.yaml",
            format!(
                "PackageLocale: en-US\nPublisher: Peder Bergan\nPublisherUrl: https://pederbe.dev\nPackageName: udpstp\nPackageUrl: https://udpstp.com\nLicense: MIT OR Apache-2.0\nLicenseUrl: https://github.com/pederbe/udpstp/blob/v{version}/LICENSE-MIT\nShortDescription: Measure network-path capacity, loss and delay\nMoniker: udpstp\nReleaseNotesUrl: https://github.com/pederbe/udpstp/releases/tag/v{version}\n"
            ),
        ),
    ];
    for (kind, name, fields) in files {
        let text = format!(
            "# yaml-language-server: $schema=https://aka.ms/winget-manifest.{kind}.1.12.0.schema.json\n{identity}{fields}ManifestType: {kind}\nManifestVersion: 1.12.0\n"
        );
        fs::write(directory.join(name), text).map_err(|e| e.to_string())?;
    }
    println!("{}", directory.display());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn portable_manifest_uses_the_selected_release_and_exact_digest() {
        let root = std::env::temp_dir().join(format!("udpstp-winget-{}", std::process::id()));
        fs::create_dir(&root).unwrap();
        let exe = root.join("udpstp.exe");
        fs::write(&exe, b"MZfixture").unwrap();
        run(&[
            "1.2.3".into(),
            exe.to_str().unwrap().into(),
            root.to_str().unwrap().into(),
        ])
        .unwrap();
        let directory = root.join("manifests/p/pederbe/udpstp/1.2.3");
        let installer =
            fs::read_to_string(directory.join("pederbe.udpstp.installer.yaml")).unwrap();
        assert!(installer.contains(
            "InstallerSha256: 78A83403FCB3B7A3FC1DF1ABBEB6DFAD652C01E561EFD9BA6985F7C75C44245A"
        ));
        assert!(installer.contains("releases/download/v1.2.3/udpstp.exe"));
        assert!(installer.contains("InstallerType: portable"));
        assert_eq!(fs::read_dir(directory).unwrap().count(), 3);
        fs::write(&exe, b"not an executable").unwrap();
        assert!(
            run(&[
                "1.2.3".into(),
                exe.to_str().unwrap().into(),
                root.to_str().unwrap().into()
            ])
            .is_err()
        );
        fs::remove_dir_all(root).unwrap();
    }
}
