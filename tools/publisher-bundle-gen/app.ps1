# -Platform app of tools\make-publisher-bundle.ps1: New-AppFiles, the creds
# and viewer.pub for a PC with the cc-usage app. Dot-sourced by that script;
# uses its parameters and variables ($Alias, $Out, $ViewerPub, $User,
# $CredsName, $DataDir, $Servers, $Bucket).

# -Platform app: the files a PC with the cc-usage app needs to publish in
# the Publisher role. The app runs cc-publish's pipeline itself and makes
# its own key pair there (Settings > Generate key), so this is the creds of
# the alias and this viewer's public key, nothing else.
function New-AppFiles {
    $folderName = "cc-usage-publisher-$Alias-app"
    $folder = Join-Path $Out $folderName
    Write-Step "app publisher files for alias '$Alias' (NATS user $User): creds and viewer.pub, no private key"

    # 4. NATS user
    $credsFile = Invoke-Provision

    # 5. the viewer's public key: -ViewerPub, else the file the app here
    # keeps in step with keys\viewer.key, else made with cc-publish
    # viewer-keygen --if-missing.
    $localPub = Join-Path $DataDir 'keys\viewer.pub'
    if ($ViewerPub) {
        $viewerPubPath = Get-ViewerPub $null
    } elseif (Test-Path -LiteralPath $localPub -PathType Leaf) {
        Assert-ViewerHere
        $viewerPubPath = $localPub
        $n = Test-KeyFile $viewerPubPath
        Write-Host "  viewer key: $viewerPubPath ($n key(s); viewer.key next to it never leaves this machine)"
    } else {
        Add-CargoToPath
        $viewerPubPath = Get-ViewerPub (Get-HostPublishExe $null)
    }
    $pin = Join-Path $DataDir "keys\publishers\$Alias.pub"
    if (-not $ViewerPub -and (Test-Path -LiteralPath $pin)) {
        Write-Warn "$pin already pins key(s) for ${Alias}: the viewer holds back the app's values until its new key is accepted here (compare it with the line Settings > Generate key shows)"
    }

    # 6. assemble
    Write-Step "assembling $folder"
    New-Item -ItemType Directory -Force -Path $Out | Out-Null
    if (Test-Path $folder) { Remove-Item -Recurse -Force $folder }
    New-Item -ItemType Directory -Force -Path $folder | Out-Null
    Set-PrivateAcl $folder
    Copy-Item -LiteralPath $credsFile -Destination (Join-Path $folder $CredsName)
    Copy-Item -LiteralPath $viewerPubPath -Destination (Join-Path $folder 'viewer.pub')

    # 8. summary
    Write-Host ''
    Write-Step "files ready: $folder"
    Get-ChildItem -LiteralPath $folder -File | Sort-Object Name | ForEach-Object {
        Write-Host ("  {0,-44} {1,10:N0}" -f $_.Name, $_.Length)
    }
    Write-Host ''
    Write-Host 'WARNING: the folder contains the NATS credentials of' -ForegroundColor Yellow
    Write-Host "  $User. Move it over a private channel only (your own USB stick, a" -ForegroundColor Yellow
    Write-Host '  private share; not e-mail, chat or a shared drive), and delete it on both' -ForegroundColor Yellow
    Write-Host '  machines once the app publishes.' -ForegroundColor Yellow
    Write-Host ''
    Write-Host "On the PC with the cc-usage app (its machine name must be $Alias):"
    Write-Host "  1. copy the folder $folderName there"
    Write-Host "  2. cc-usage > Settings > This machine's name: check it reads $Alias"
    if (($Servers -join ',') -ne ($DefaultNatsServers -join ',') -or $Bucket -ne $DefaultNatsBucket) {
        $natsJson = '"nats": {"servers": [' + (($Servers | ForEach-Object { "`"$_`"" }) -join ', ') + "], `"bucket`": `"$Bucket`"}"
        Write-Host "     and its settings.json (%APPDATA%\$AppIdentifier, app closed) holds the same NATS settings:"
        Write-Host "     $natsJson"
    }
    Write-Host '  3. Settings > Role: Publisher > Switch and restart'
    Write-Host "  4. Settings > Import creds... > $CredsName"
    Write-Host '  5. Settings > Import viewer key... > viewer.pub'
    Write-Host '  6. Settings > Generate key (this viewer asks to accept its public line)'
    Write-Host '  7. the three checks turn green and publishing starts within 30 s (or restart cc-usage);'
    Write-Host "     the footer reads: publishing as $Alias - connected"
    Write-Host '  8. delete the copied folder'
    Write-Host "Here, the viewer shows $Alias's sessions with its next heartbeat."
}
