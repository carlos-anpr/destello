# Instala Destello para el usuario actual (sin administrador).
#   powershell -ExecutionPolicy Bypass -File installer\install.ps1 [-Exe <ruta>]
# Desde el ZIP de la release basta con doble clic en Instalar.cmd.
# Reejecutarlo actualiza una instalación existente.
param([string]$Exe)
$ErrorActionPreference = 'Stop'

if (-not $Exe) {
    # ZIP de la release: destello.exe junto a este script. Repo: el binario de cargo build --release.
    $Exe = Join-Path $PSScriptRoot 'destello.exe'
    if (-not (Test-Path $Exe)) { $Exe = Join-Path $PSScriptRoot '..\target\release\destello.exe' }
}
$Exe = (Resolve-Path $Exe).Path
$dir = Join-Path $env:LOCALAPPDATA 'Programs\Destello'
$target = Join-Path $dir 'destello.exe'
$version = (Get-Item $Exe).VersionInfo.ProductVersion
if (-not $version) { $version = '1.0.0' }

# 1. Cerrar instancias en ejecución (incluida la de target\release)
Get-Process destello -ErrorAction SilentlyContinue | ForEach-Object {
    $_.CloseMainWindow() | Out-Null
    if (-not $_.WaitForExit(2000)) { $_ | Stop-Process -Force }
}
Start-Sleep -Milliseconds 300

# 2. Copiar binario y desinstalador
New-Item -ItemType Directory -Force $dir | Out-Null
Copy-Item $Exe $target -Force
Copy-Item (Join-Path $PSScriptRoot 'uninstall.ps1') (Join-Path $dir 'uninstall.ps1') -Force
# Lo descargado de Internet lleva la marca Zone.Identifier: sin quitarla, Windows pediría
# confirmación cada vez que Destello arranca con la sesión.
Unblock-File $target
Unblock-File (Join-Path $dir 'uninstall.ps1')

# 3. Acceso directo en el menú Inicio
$programs = [Environment]::GetFolderPath('Programs')
$shell = New-Object -ComObject WScript.Shell
$lnk = $shell.CreateShortcut((Join-Path $programs 'Destello.lnk'))
$lnk.TargetPath = $target
$lnk.WorkingDirectory = $dir
$lnk.IconLocation = "$target,0"
$lnk.Description = 'Capturador de pantalla'
$lnk.Save()

# 4. Arranque con Windows apuntando a la ruta instalada
Set-ItemProperty 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Run' -Name 'Destello' -Value "`"$target`""

# 5. PATH de usuario (para usar `destello full`, `destello window`... desde cualquier terminal)
$userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
$parts = @($userPath -split ';' | Where-Object { $_ -ne '' })
if ($parts -notcontains $dir) {
    [Environment]::SetEnvironmentVariable('Path', (($parts + $dir) -join ';'), 'User')
}

# 6. Entrada en Configuración > Aplicaciones instaladas
$key = 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall\Destello'
New-Item $key -Force | Out-Null
$sizeKb = [int]((Get-ChildItem $dir -File | Measure-Object Length -Sum).Sum / 1KB)
$props = @{
    DisplayName     = 'Destello'
    DisplayVersion  = $version
    Publisher       = 'Destello'
    DisplayIcon     = "$target,0"
    InstallLocation = $dir
    UninstallString = "powershell.exe -NoProfile -ExecutionPolicy Bypass -WindowStyle Hidden -File `"$dir\uninstall.ps1`""
    InstallDate     = (Get-Date -Format 'yyyyMMdd')
}
foreach ($p in $props.GetEnumerator()) { Set-ItemProperty $key -Name $p.Key -Value $p.Value }
Set-ItemProperty $key -Name NoModify -Value 1 -Type DWord
Set-ItemProperty $key -Name NoRepair -Value 1 -Type DWord
Set-ItemProperty $key -Name EstimatedSize -Value $sizeKb -Type DWord

# 7. Arrancar en la bandeja
Start-Process $target -WorkingDirectory $dir
Write-Output "Destello $version instalado en $dir"
