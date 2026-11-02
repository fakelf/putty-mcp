$ErrorActionPreference='Stop'
$taskRoot=Split-Path -Parent $PSScriptRoot
$manifest=Join-Path $taskRoot 'putty-ai-rs/Cargo.toml'
$rawMetadata=& cargo metadata --locked --format-version 1 --filter-platform x86_64-pc-windows-msvc --manifest-path $manifest
if($LASTEXITCODE) { throw 'Cannot inspect dependency licenses' }
$metadata=$rawMetadata | ConvertFrom-Json
$parts=[Collections.Generic.List[string]]::new()
$parts.Add('Rust dependency license notices. PuTTY license is separately included as LICENCE. Generated from locked package sources.')
foreach($dependency in ($metadata.packages | Where-Object source | Sort-Object name,version)) {
    $parts.Add("`n===== $($dependency.name) $($dependency.version) [$($dependency.license)] =====`n")
    $packageDirectory=Split-Path -Parent $dependency.manifest_path
    $licenseFiles=@(Get-ChildItem -LiteralPath $packageDirectory -File | Where-Object Name -Match '^(LICENSE|LICENCE|COPYING|NOTICE)([._-].*)?$')
    if($dependency.license_file) {
        $custom=Join-Path $packageDirectory $dependency.license_file
        if(Test-Path -LiteralPath $custom) { $licenseFiles+=Get-Item -LiteralPath $custom }
    }
    foreach($license in ($licenseFiles | Sort-Object FullName -Unique)) {
        $parts.Add("--- $($license.Name) ---`n"+(Get-Content -LiteralPath $license.FullName -Raw))
    }
}
[IO.File]::WriteAllText((Join-Path $taskRoot 'docs/third-party-licenses.txt'),($parts -join "`n"),[Text.UTF8Encoding]::new($false))
