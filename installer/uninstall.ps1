# Desinstala Destello del usuario actual. Lo invoca Configuración > Aplicaciones instaladas.
$ErrorActionPreference = 'SilentlyContinue'
$dir = Join-Path $env:LOCALAPPDATA 'Programs\Destello'

Get-Process destello | ForEach-Object {
    $_.CloseMainWindow() | Out-Null
    if (-not $_.WaitForExit(2000)) { $_ | Stop-Process -Force }
}
Start-Sleep -Milliseconds 300

Remove-ItemProperty 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Run' -Name 'Destello'
Remove-Item (Join-Path ([Environment]::GetFolderPath('Programs')) 'Destello.lnk') -Force
Remove-Item 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall\Destello' -Recurse -Force

$userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
$parts = @($userPath -split ';' | Where-Object { $_ -ne '' -and $_ -ne $dir })
[Environment]::SetEnvironmentVariable('Path', ($parts -join ';'), 'User')

# Devolver ImpPnt a Recortes de Windows (Destello lo había desactivado)
Set-ItemProperty 'HKCU:\Control Panel\Keyboard' -Name PrintScreenKeyForSnippingEnabled -Value 1 -Type DWord

# Respaldo temporal del portapapeles y configuración
Remove-Item (Join-Path $env:TEMP 'Destello') -Recurse -Force
Remove-Item (Join-Path $env:APPDATA 'Destello') -Recurse -Force
Remove-Item (Join-Path $env:LOCALAPPDATA 'Destello') -Recurse -Force

# El propio directorio de instalación (incluido este script) al final
Set-Location $env:TEMP
Remove-Item $dir -Recurse -Force
