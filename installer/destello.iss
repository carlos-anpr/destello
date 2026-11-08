; Instalador de Destello (Inno Setup 6). Instala solo para el usuario actual, sin administrador.
;   cargo build --release
;   "C:\Program Files (x86)\Inno Setup 6\ISCC.exe" /DAppVersion=1.0.0 installer\destello.iss
; Resultado: dist\Destello_<versión>_x64-setup.exe

#ifndef AppVersion
  #error Falta /DAppVersion=X.Y.Z
#endif
#ifndef SourceExe
  #define SourceExe "..\target\release\destello.exe"
#endif

[Setup]
AppId={{4E678A9A-0FA4-4EA3-82CD-C30CBCB3D763}
AppName=Destello
AppVersion={#AppVersion}
AppVerName=Destello {#AppVersion}
AppPublisher=Destello
AppPublisherURL=https://github.com/carlos-anpr/destello
AppSupportURL=https://github.com/carlos-anpr/destello/issues
AppUpdatesURL=https://github.com/carlos-anpr/destello/releases
AppComments=Capturador de pantalla para Windows
VersionInfoVersion={#AppVersion}.0
VersionInfoDescription=Instalador de Destello
; Por usuario: {autopf} = %LOCALAPPDATA%\Programs y el registro va a HKCU
PrivilegesRequired=lowest
DefaultDirName={autopf}\Destello
DisableDirPage=auto
DisableProgramGroupPage=yes
DisableReadyPage=yes
UsePreviousTasks=yes
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
MinVersion=10.0
OutputDir=..\dist
OutputBaseFilename=Destello_{#AppVersion}_x64-setup
SetupIconFile=..\assets\destello.ico
UninstallDisplayIcon={app}\destello.exe
UninstallDisplayName=Destello
Compression=lzma2/max
SolidCompression=yes
WizardStyle=modern
ChangesEnvironment=yes
; Destello vive en la bandeja: el instalador lo cierra él mismo (PrepareToInstall)
CloseApplications=no
SetupMutex=Destello.Setup

[Languages]
Name: "es"; MessagesFile: "compiler:Languages\Spanish.isl"

[CustomMessages]
es.TaskAutostart=Iniciar Destello con Windows (en la bandeja del sistema)
es.TaskPath=Añadir el comando destello al PATH (para usarlo desde la terminal)
es.TaskDesktop=Crear un acceso directo en el escritorio
es.RunNow=Iniciar Destello ahora

[Tasks]
Name: "autostart"; Description: "{cm:TaskAutostart}"
Name: "path"; Description: "{cm:TaskPath}"
Name: "desktop"; Description: "{cm:TaskDesktop}"; Flags: unchecked

[InstallDelete]
; Restos de la instalación anterior por script (install.ps1)
Type: files; Name: "{app}\uninstall.ps1"

[Files]
Source: "{#SourceExe}"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{autoprograms}\Destello"; Filename: "{app}\destello.exe"; Comment: "Capturador de pantalla"
Name: "{autodesktop}\Destello"; Filename: "{app}\destello.exe"; Comment: "Capturador de pantalla"; Tasks: desktop

[Registry]
Root: HKCU; Subkey: "Software\Microsoft\Windows\CurrentVersion\Run"; ValueType: string; ValueName: "Destello"; ValueData: """{app}\destello.exe"""; Flags: uninsdeletevalue; Tasks: autostart
; La entrada de "Aplicaciones instaladas" del antiguo install.ps1 (ahora la gestiona este instalador)
Root: HKCU; Subkey: "Software\Microsoft\Windows\CurrentVersion\Uninstall\Destello"; Flags: deletekey dontcreatekey
Root: HKCU; Subkey: "Environment"; ValueType: expandsz; ValueName: "Path"; ValueData: "{olddata};{app}"; Check: NeedsAddPath(ExpandConstant('{app}')); Tasks: path

[Run]
Filename: "{app}\destello.exe"; Description: "{cm:RunNow}"; Flags: nowait postinstall skipifsilent

[UninstallDelete]
; Respaldo temporal del portapapeles, configuración y registros
Type: filesandordirs; Name: "{%TEMP}\Destello"
Type: filesandordirs; Name: "{userappdata}\Destello"
Type: filesandordirs; Name: "{localappdata}\Destello"
Type: dirifempty; Name: "{app}"

[Code]
procedure CloseDestello();
var
  Code: Integer;
begin
  // Primero con WM_CLOSE (cierre limpio) y, si sigue vivo, a la fuerza
  Exec(ExpandConstant('{sys}\taskkill.exe'), '/IM destello.exe', '', SW_HIDE, ewWaitUntilTerminated, Code);
  Sleep(800);
  Exec(ExpandConstant('{sys}\taskkill.exe'), '/F /IM destello.exe', '', SW_HIDE, ewWaitUntilTerminated, Code);
  Sleep(300);
end;

function PrepareToInstall(var NeedsRestart: Boolean): String;
begin
  CloseDestello();
  Result := '';
end;

function InitializeUninstall(): Boolean;
begin
  CloseDestello();
  Result := True;
end;

function SplitContains(const List, Item: String): Boolean;
begin
  Result := Pos(';' + Lowercase(Item) + ';', ';' + Lowercase(List) + ';') > 0;
end;

function NeedsAddPath(Dir: String): Boolean;
var
  Current: String;
begin
  if not RegQueryStringValue(HKCU, 'Environment', 'Path', Current) then
    Result := True
  else
    Result := not SplitContains(Current, Dir) and not SplitContains(Current, Dir + '\');
end;

procedure RemoveFromPath(Dir: String);
var
  Current, Rest, Part, Kept: String;
  P: Integer;
begin
  if not RegQueryStringValue(HKCU, 'Environment', 'Path', Current) then
    Exit;
  Rest := Current;
  Kept := '';
  while Rest <> '' do
  begin
    P := Pos(';', Rest);
    if P = 0 then
    begin
      Part := Rest;
      Rest := '';
    end else
    begin
      Part := Copy(Rest, 1, P - 1);
      Delete(Rest, 1, P);
    end;
    if (Part <> '') and (CompareText(Part, Dir) <> 0) and (CompareText(Part, Dir + '\') <> 0) then
    begin
      if Kept <> '' then
        Kept := Kept + ';';
      Kept := Kept + Part;
    end;
  end;
  if Kept <> Current then
    RegWriteExpandStringValue(HKCU, 'Environment', 'Path', Kept);
end;

procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
begin
  if CurUninstallStep = usPostUninstall then
  begin
    RemoveFromPath(ExpandConstant('{app}'));
    // Devolver ImpPnt a Recortes de Windows (Destello lo desactiva mientras está instalado)
    RegWriteDWordValue(HKCU, 'Control Panel\Keyboard', 'PrintScreenKeyForSnippingEnabled', 1);
  end;
end;
