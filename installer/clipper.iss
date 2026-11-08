; Assistant d'installation de Clipper (Inno Setup 6).
; Compiler : cargo build --release, une fois powershell -File installer\fetch-presentmon.ps1, puis
;   ISCC.exe /DBin=D:\ct\release installer\clipper.iss
; Résultat : installer\out\Clipper-Setup-<version>.exe

#ifndef Bin
  #define Bin "..\target\release"
#endif
#define AppVersion "1.2.3"

[Setup]
AppId={{6B1E3F52-8C0A-4C1D-9E57-3A1C2F0B7D41}
AppName=Clipper
AppVersion={#AppVersion}
AppPublisher=LxL-952
AppPublisherURL=https://github.com/LxL-952/clipper
AppSupportURL=https://github.com/LxL-952/clipper/issues
; Installation pour l'utilisateur seul : aucune demande administrateur.
PrivilegesRequired=lowest
DefaultDirName={localappdata}\Programs\Clipper
DefaultGroupName=Clipper
DisableProgramGroupPage=yes
LicenseFile=conditions.txt
SetupIconFile={#Bin}\clipper.ico
UninstallDisplayIcon={app}\clipper.exe
OutputDir=out
OutputBaseFilename=Clipper-Setup-{#AppVersion}
Compression=lzma2/max
SolidCompression=yes
WizardStyle=modern
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
MinVersion=10.0.17763
; Ferme Clipper s'il tourne pendant une mise à jour ou une désinstallation.
CloseApplications=yes

[Languages]
Name: "fr"; MessagesFile: "compiler:Languages\French.isl"

[Tasks]
Name: "desktop"; Description: "Créer une icône sur le Bureau"; GroupDescription: "Raccourcis :"
Name: "startup"; Description: "Lancer Clipper au démarrage de Windows (dans la zone de notification)"; GroupDescription: "Démarrage :"
Name: "ffmpeg"; Description: "Installer FFmpeg avec winget (nécessaire pour enregistrer)"; GroupDescription: "Composants :"; Check: NeedsFfmpeg

[Files]
Source: "{#Bin}\clipper.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\LICENSE"; DestDir: "{app}"; DestName: "LICENSE.txt"
; PresentMon d'Intel (licence MIT) : FPS de l'overlay pour DirectX, Vulkan et OpenGL. Voir fetch-presentmon.ps1.
Source: "vendor\presentmon.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "third-party\PresentMon-LICENSE.txt"; DestDir: "{app}\third-party"

[Icons]
Name: "{group}\Clipper"; Filename: "{app}\clipper.exe"
Name: "{group}\Désinstaller Clipper"; Filename: "{uninstallexe}"
Name: "{userdesktop}\Clipper"; Filename: "{app}\clipper.exe"; Tasks: desktop

[Registry]
Root: HKCU; Subkey: "Software\Microsoft\Windows\CurrentVersion\Run"; ValueType: string; ValueName: "Clipper"; ValueData: """{app}\clipper.exe"" --tray"; Flags: uninsdeletevalue; Tasks: startup

[Run]
Filename: "winget.exe"; Parameters: "install -e --id Gyan.FFmpeg --scope user --accept-package-agreements --accept-source-agreements"; StatusMsg: "Installation de FFmpeg…"; Flags: runhidden waituntilterminated; Tasks: ffmpeg
Filename: "{app}\clipper.exe"; Description: "Lancer Clipper"; Flags: nowait postinstall skipifsilent
; Mise à jour depuis Clipper (mode silencieux) : relancé tout seul, réduit dans la zone de notification.
Filename: "{app}\clipper.exe"; Parameters: "--tray"; Flags: nowait; Check: WizardSilent

[UninstallDelete]
; Réglages et journal. Les clips (dossier Vidéos) ne sont jamais touchés.
Type: filesandordirs; Name: "{userappdata}\Clipper"
Type: filesandordirs; Name: "{localappdata}\Temp\Clipper"

[Code]
{ FFmpeg déjà dans le PATH ? Sinon la case d'installation est proposée. }
function NeedsFfmpeg: Boolean;
var
  Code: Integer;
begin
  Result := not (Exec(ExpandConstant('{cmd}'), '/c where ffmpeg >nul 2>nul', '', SW_HIDE, ewWaitUntilTerminated, Code) and (Code = 0));
end;

{ Clipper vit dans la zone de notification, sans fenêtre : Inno ne sait pas le fermer seul.
  On lui demande de quitter (--quit), puis on laisse ffmpeg et PresentMon s'arrêter avec lui. }
procedure QuitClipper(Exe: String);
var
  Code: Integer;
begin
  if FileExists(Exe) then
  begin
    Exec(Exe, '--quit', '', SW_HIDE, ewWaitUntilTerminated, Code);
    if Code = 0 then
      Sleep(2000);
  end;
end;

function PrepareToInstall(var NeedsRestart: Boolean): String;
begin
  QuitClipper(ExpandConstant('{app}\clipper.exe'));
  Result := '';
end;

function InitializeUninstall: Boolean;
begin
  QuitClipper(ExpandConstant('{app}\clipper.exe'));
  Result := True;
end;
