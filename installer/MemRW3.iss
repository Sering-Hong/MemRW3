#ifndef PayloadDir
  #error PayloadDir must point to the staged portable edition
#endif
#ifndef OutputDir
  #define OutputDir "..\dist\installer"
#endif
#define AppVersion "2.2.1-pink.1"

[Setup]
AppId={{59C6A927-5D9C-4E41-80B3-BC623BB3C81B}
AppName=MemRW3
AppVersion={#AppVersion}
AppPublisher=Sering-Hong
AppPublisherURL=https://github.com/Sering-Hong/MemRW3
AppSupportURL=https://github.com/Sering-Hong/MemRW3/issues
DefaultDirName={code:DefaultInstallDirectory}
DefaultGroupName=MemRW3
PrivilegesRequired=lowest
ArchitecturesAllowed=x64compatible and not arm64
ArchitecturesInstallIn64BitMode=x64compatible
MinVersion=10.0.17763
OutputDir={#OutputDir}
OutputBaseFilename=MemRW3-Setup-{#AppVersion}-x64
SetupIconFile=..\assets\MemRW3-pink.ico
UninstallDisplayIcon={app}\MemRW3.exe
VersionInfoVersion=2.2.1.1
Compression=lzma2
SolidCompression=yes
WizardStyle=modern
WizardResizable=no
DisableWelcomePage=yes
DisableDirPage=no
DisableProgramGroupPage=yes
DisableReadyPage=yes
DirExistsWarning=no
UsePreviousTasks=yes
CloseApplications=yes
RestartApplications=no
SetupLogging=yes
UninstallDisplayName=MemRW3 Enhanced Pink

[Languages]
Name: "zhcn"; MessagesFile: "ChineseSimplified.isl"

[Messages]
zhcn.SelectDirLabel3=安装程序将检测运行环境，并将 [name] 安装到下面的目录。
zhcn.SelectDirDesc=确认安装目录后，点击“安装”即可。
zhcn.ButtonNext=安装(&I)
zhcn.WindowsVersionNotSupported=此安装包需要 Windows 10 1809 或更新的 x64 系统（支持 Windows 11），不支持 32 位或 ARM64 系统。

[Tasks]
Name: "desktopicon"; Description: "创建粉色桌面快捷方式"; Flags: checkedonce
Name: "startmenuicon"; Description: "创建开始菜单快捷方式"; Flags: checkedonce

[Files]
; Bootstrap and the actual program are checked before any installation files are written.
Source: "{#PayloadDir}\MemRW3-runtime-check.exe"; Flags: dontcopy
Source: "{#PayloadDir}\runtime-dependencies.txt"; Flags: dontcopy
Source: "{#PayloadDir}\MemRW3.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#PayloadDir}\assets\*"; DestDir: "{app}\assets"; Flags: ignoreversion
Source: "{#PayloadDir}\使用说明.md"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\docs\安装说明.md"; DestDir: "{app}"; DestName: "安装说明.md"; Flags: ignoreversion
Source: "{#PayloadDir}\项目技术文档.md"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#PayloadDir}\CHANGELOG.md"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#PayloadDir}\package-info.json"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#PayloadDir}\MemRW3-runtime-check.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#PayloadDir}\runtime-dependencies.txt"; DestDir: "{app}"; Flags: ignoreversion

Source: "InnoSetup-LICENSE.txt"; DestDir: "{app}\licenses"; Flags: ignoreversion
Source: "THIRD_PARTY.md"; DestDir: "{app}\licenses"; Flags: ignoreversion

[Icons]
Name: "{userdesktop}\MemRW3"; Filename: "{app}\MemRW3.exe"; WorkingDir: "{app}"; IconFilename: "{app}\assets\MemRW3-pink.ico"; Tasks: desktopicon
Name: "{userprograms}\MemRW3\MemRW3"; Filename: "{app}\MemRW3.exe"; WorkingDir: "{app}"; IconFilename: "{app}\assets\MemRW3-pink.ico"; Tasks: startmenuicon
Name: "{userprograms}\MemRW3\安装说明"; Filename: "{app}\安装说明.md"; Tasks: startmenuicon
Name: "{userprograms}\MemRW3\卸载 MemRW3"; Filename: "{uninstallexe}"; Tasks: startmenuicon

[Run]
Filename: "{app}\MemRW3.exe"; Description: "启动 MemRW3"; Flags: nowait postinstall skipifsilent

[Code]
function DefaultInstallDirectory(Param: String): String;
begin
  if DirExists('D:\') then
    Result := 'D:\MemRW3'
  else
    Result := ExpandConstant('{localappdata}\Programs\MemRW3');
end;

function ShouldSkipPage(PageID: Integer): Boolean;
begin
  Result := PageID = wpSelectTasks;
end;

function PrepareToInstall(var NeedsRestart: Boolean): String;
var
  Code: Integer;
  RawReport: AnsiString;
  Details, ReportPath, SavedReport, Parameters: String;
begin
  Result := '';
  ExtractTemporaryFile('MemRW3-runtime-check.exe');
  ExtractTemporaryFile('runtime-dependencies.txt');
  ExtractTemporaryFile('MemRW3.exe');
  ReportPath := ExpandConstant('{tmp}\MemRW3-dependencies.txt');
  Parameters := '"' + ExpandConstant('{tmp}\MemRW3.exe') + '" "' +
    ExpandConstant('{tmp}\runtime-dependencies.txt') + '" "' + ReportPath + '"';
  if not Exec(ExpandConstant('{tmp}\MemRW3-runtime-check.exe'), Parameters,
      ExpandConstant('{tmp}'), SW_HIDE, ewWaitUntilTerminated, Code) then
  begin
    Result := '无法启动运行环境检查工具。系统错误：' + SysErrorMessage(Code) + #13#10 +
      '请检查 Windows 更新、安全软件拦截及当前用户的临时目录权限。';
    Exit;
  end;
  Details := '';
  if LoadStringFromFile(ReportPath, RawReport) then
    Details := UTF8Decode(RawReport);
  Log('Dependency check exit code: ' + IntToStr(Code) + '. ' + Details);
  SavedReport := ExpandConstant('{localappdata}\MemRW3\install-dependencies.txt');
  if ForceDirectories(ExtractFileDir(SavedReport)) then
    FileCopy(ReportPath, SavedReport, False);
  if Code <> 0 then
  begin
    Result := '运行环境检查未通过，尚未安装程序。' + #13#10 + #13#10 + Details + #13#10 +
      '缺少 api-ms-win-* / ucrtbase / 系统 DLL：请更新或修复 Windows。' + #13#10 +
      '缺少 VCRUNTIME140 / MSVCP140：请安装 Microsoft Visual C++ x64 运行库：' + #13#10 +
      'https://aka.ms/vc14/vc_redist.x64.exe' + #13#10 +
      '图形初始化失败：请更新显卡驱动；远程桌面/虚拟机请启用图形加速。' + #13#10 +
      '修复后重新运行本安装包。检查报告：' + SavedReport;
    Log(Result);
  end;
end;
