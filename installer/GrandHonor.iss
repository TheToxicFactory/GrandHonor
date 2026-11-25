#ifndef AppVersion
  #error AppVersion must be supplied by build.ps1
#endif
#ifndef PayloadDir
  #error PayloadDir must be supplied by build.ps1
#endif

[Setup]
AppId=GrandHonor
AppName=GrandHonor
AppVersion={#AppVersion}
AppVerName=GrandHonor {#AppVersion}
AppPublisher=GrandHonor
DefaultDirName={commonpf64}\GrandHonor
DisableDirPage=yes
DisableProgramGroupPage=yes
DisableWelcomePage=no
DisableReadyPage=no
UsePreviousAppDir=no
PrivilegesRequired=admin
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
MinVersion=10.0.22000
UninstallDisplayName=GrandHonor
UninstallDisplayIcon={app}\GrandHonor.ico
UninstallFilesDir={app}
CloseApplications=yes
CloseApplicationsFilter=*.exe
RestartApplications=no
Compression=lzma2/ultra64
SolidCompression=yes
LZMANumFastBytes=273
OutputDir=..\dist
OutputBaseFilename=GrandHonor-Setup-x64
SetupIconFile=..\Assets\GrandHonor.ico
WizardStyle=modern dynamic windows11 includetitlebar hidebevels
WizardBackColor=#F5F5F7
WizardBackColorDynamicDark=#121214
WizardSizePercent=108,104
#ifdef BrandDir
WizardImageFile={#BrandDir}\hero-light.png
WizardImageFileDynamicDark={#BrandDir}\hero-dark.png
WizardSmallImageFile={#BrandDir}\mark-light.png
WizardSmallImageFileDynamicDark={#BrandDir}\mark-dark.png
#endif

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"
Name: "russian"; MessagesFile: "compiler:Languages\Russian.isl"

[Messages]
english.WelcomeLabel1=GrandHonor {#AppVersion}
russian.WelcomeLabel1=GrandHonor {#AppVersion}
english.WelcomeLabel2=A light, ad-free replacement for HONOR PC Manager: sensors, power modes, charge limits, keyboard light, touchpad gestures and Fn keys, from the Windows tray.%n%nGrandHonor will be installed in Program Files. Setup works offline.
russian.WelcomeLabel2=Лёгкая замена HONOR PC Manager без рекламы: датчики, режимы питания, лимиты заряда, подсветка, жесты тачпада и клавиши Fn — из трея Windows.%n%nGrandHonor будет установлен в Program Files. Подключение к интернету не требуется.
english.ConfirmUninstall=Remove GrandHonor from this PC? Your settings will be kept unless you choose to clear them.
russian.ConfirmUninstall=Удалить GrandHonor с этого компьютера? Настройки сохранятся, если вы не выберете их очистку.

[CustomMessages]
english.StartupTask=Start GrandHonor with Windows (recommended: Fn keys, touchpad gestures and charge limits work only while it runs)
russian.StartupTask=Запускать GrandHonor вместе с Windows (рекомендуется: клавиши Fn, жесты тачпада и лимиты заряда работают, только пока он запущен)
english.ShortcutComment=Fn keys, sensors, power and battery for HONOR MagicBook
russian.ShortcutComment=Клавиши Fn, датчики, питание и батарея для HONOR MagicBook
english.StartupStatus=Registering GrandHonor to start with Windows...
russian.StartupStatus=Включение автозапуска GrandHonor...

[Files]
; Inter (SIL OFL 1.1) for the wizard text: unpacked to {tmp} and loaded privately, never installed.
Source: "..\Assets\fonts\Inter-Regular.ttf"; Flags: dontcopy
Source: "..\Assets\fonts\Inter-SemiBold.ttf"; Flags: dontcopy
Source: "..\Assets\fonts\InterDisplay-SemiBold.ttf"; Flags: dontcopy

[Tasks]
Name: "startup"; Description: "{cm:StartupTask}"
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; Flags: unchecked

[Files]
Source: "{#PayloadDir}\GrandHonor.exe"; DestDir: "{app}"; Flags: ignoreversion; Check: CopyHost
Source: "{#PayloadDir}\GrandHonor.Panel.exe"; DestDir: "{app}"; Flags: ignoreversion; Check: CopyPanel
Source: "{#PayloadDir}\*"; DestDir: "{app}"; Excludes: "GrandHonor.exe,GrandHonor.Panel.exe"; Flags: ignoreversion recursesubdirs skipifsourcedoesntexist

[Icons]
; Shortcut names carry the version; the AppUserModelID matches the one GrandHonor.exe sets,
; so Windows ties notifications, Start search and taskbar pins to this app.
Name: "{commonprograms}\GrandHonor {#AppVersion}"; Filename: "{app}\GrandHonor.exe"; WorkingDir: "{app}"; IconFilename: "{app}\GrandHonor.ico"; AppUserModelID: "GrandHonor.App"; Comment: "{cm:ShortcutComment}"
Name: "{app}\GrandHonor {#AppVersion}"; Filename: "{app}\GrandHonor.exe"; WorkingDir: "{app}"; IconFilename: "{app}\GrandHonor.ico"; AppUserModelID: "GrandHonor.App"; Comment: "{cm:ShortcutComment}"
Name: "{autodesktop}\GrandHonor {#AppVersion}"; Filename: "{app}\GrandHonor.exe"; WorkingDir: "{app}"; IconFilename: "{app}\GrandHonor.ico"; AppUserModelID: "GrandHonor.App"; Comment: "{cm:ShortcutComment}"; Tasks: desktopicon

[Registry]
; App Paths: Windows Search and Win+R find "GrandHonor" whatever the shortcut is called.
Root: HKLM; Subkey: "Software\Microsoft\Windows\CurrentVersion\App Paths\GrandHonor.exe"; ValueType: string; ValueName: ""; ValueData: "{app}\GrandHonor.exe"; Flags: uninsdeletekey
Root: HKLM; Subkey: "Software\Microsoft\Windows\CurrentVersion\App Paths\GrandHonor.exe"; ValueType: string; ValueName: "Path"; ValueData: "{app}"

[InstallDelete]
; Shortcuts of earlier versions (their names contain the old version number).
Type: files; Name: "{commonprograms}\GrandHonor*.lnk"
Type: files; Name: "{app}\GrandHonor*.lnk"
Type: files; Name: "{autodesktop}\GrandHonor*.lnk"

[Run]
Filename: "{app}\GrandHonor.exe"; Parameters: "--install-privileged-tasks"; WorkingDir: "{app}"; Tasks: startup; StatusMsg: "{cm:StartupStatus}"; Flags: runhidden waituntilterminated
Filename: "{app}\GrandHonor.exe"; Description: "{cm:LaunchProgram,GrandHonor}"; Flags: runascurrentuser nowait postinstall skipifsilent

[Code]
const
  AppReg = 'Software\Microsoft\Windows\CurrentVersion\Uninstall\GrandHonor_is1';
  WM_CLOSE = $0010;

var
  InstalledVersion: String;
  VersionOrder: Integer;
  OperationPage: TInputOptionWizardPage;
  Removed: Boolean;
  { The uninstall progress form is already destroyed at usPostUninstall, so the completion
    dialog keeps its own copy of the logo (taken while the form still exists). }
  UninstallLogo: TBitmap;

function GetFileAttributes(FileName: String): LongWord;
  external 'GetFileAttributesW@kernel32.dll stdcall';

function FindWindowEx(Parent, ChildAfter: HWND; ClassName, WindowName: String): HWND;
  external 'FindWindowExW@user32.dll stdcall';

function PostMessageWnd(Wnd: HWND; Msg: LongWord; WParam, LParam: LongInt): BOOL;
  external 'PostMessageW@user32.dll stdcall';

function GetWindowsAccent(var Color: LongWord; var OpaqueBlend: LongBool): Integer;
  external 'DwmGetColorizationColor@dwmapi.dll stdcall';

function AddFontResourceEx(FileName: String; Flags: LongWord; Reserved: LongInt): Integer;
  external 'AddFontResourceExW@gdi32.dll stdcall';

const
  FR_PRIVATE = $10;

var
  InterLoaded: Boolean;

{ Loads Inter for this process only (FR_PRIVATE): Windows forgets it when setup exits. }
procedure LoadInter;
var
  Name: String;
  I: Integer;
begin
  InterLoaded := True;
  for I := 0 to 2 do begin
    case I of
      0: Name := 'Inter-Regular.ttf';
      1: Name := 'Inter-SemiBold.ttf';
    else
      Name := 'InterDisplay-SemiBold.ttf';
    end;
    try
      ExtractTemporaryFile(Name);
      if AddFontResourceEx(ExpandConstant('{tmp}\') + Name, FR_PRIVATE, 0) = 0 then InterLoaded := False;
    except
      InterLoaded := False;
    end;
  end;
end;

{ Text in Inter, titles in Inter Display SemiBold. Without Inter the Windows 11 faces stay. }
procedure ApplyType(Body: TFont; Title: TFont; Page: TFont);
begin
  if not InterLoaded then Exit;
  Body.Name := 'Inter';
  if Title <> nil then begin
    Title.Name := 'Inter Display SemiBold';
    Title.Style := Title.Style - [fsBold];
  end;
  if Page <> nil then begin
    Page.Name := 'Inter SemiBold';
    Page.Style := Page.Style - [fsBold];
  end;
end;

function AccentChannel(Channel: LongWord; Dark: Boolean): LongWord;
begin
  Result := Channel * 3 div 5;
  if Dark then Result := Result + 102;
end;

function WindowsAccent(DefaultColor: TColor): TColor;
var
  Color, Light: LongWord;
  Ignored: LongBool;
begin
  Result := DefaultColor;
  if RegQueryDWordValue(HKCU, 'Software\Microsoft\Windows\DWM', 'AccentColor', Color) then
    Color := ((Color and 255) shl 16) or (Color and $FF00) or ((Color shr 16) and 255)
  else if GetWindowsAccent(Color, Ignored) < 0 then Exit;
  Light := 1;
  RegQueryDWordValue(HKCU, 'Software\Microsoft\Windows\CurrentVersion\Themes\Personalize', 'AppsUseLightTheme', Light);
  Result := AccentChannel((Color shr 16) and 255, Light = 0)
    or (AccentChannel((Color shr 8) and 255, Light = 0) shl 8)
    or (AccentChannel(Color and 255, Light = 0) shl 16);
end;

function Local(const Ru, En: String): String;
begin
  if ActiveLanguage = 'russian' then Result := Ru else Result := En;
end;

function FixedRoot: String;
begin
  Result := ExpandConstant('{commonpf64}\GrandHonor');
end;

procedure CloseRunningHost;
var
  Wnd: HWND;
  Retries, Code: Integer;
  WasRunning: Boolean;
begin
  Retries := 0;
  Wnd := FindWindowEx(0, 0, 'GrandHonorHostWindow', '');
  WasRunning := Wnd <> 0;
  while (Wnd <> 0) and (Retries < 10) do begin
    PostMessageWnd(Wnd, WM_CLOSE, 0, 0);
    Sleep(200);
    Wnd := FindWindowEx(0, 0, 'GrandHonorHostWindow', '');
    Retries := Retries + 1;
  end;
  { The window goes first; the process then saves and ends (2.5.3 and later within 5 s).
    GrandHonor 2.5.2 and earlier could hang after the window closed, keeping its files in
    use, so whatever is left of GrandHonor is ended. }
  if WasRunning then Sleep(1500);
  // Only the copy installed here: a portable copy elsewhere keeps running.
  Exec(ExpandConstant('{sys}\WindowsPowerShell\v1.0\powershell.exe'), '-NoProfile -NonInteractive -Command "Get-Process GrandHonor,GrandHonor.Panel -ErrorAction SilentlyContinue | Where-Object { $_.Path -like ''' + ExpandConstant('{app}') + '\*'' } | Stop-Process -Force"', '', SW_HIDE, ewWaitUntilTerminated, Code);
end;

function InitializeSetup: Boolean;
var
  Existing, FileVersion, Target: Int64;
  FileVersionText: String;
begin
  Result := True;
  VersionOrder := -1;
  RegQueryStringValue(HKLM64, AppReg, 'DisplayVersion', InstalledVersion);
  Existing := 0;
  StrToVersion(InstalledVersion, Existing);
  if GetVersionNumbersString(FixedRoot + '\GrandHonor.exe', FileVersionText) and
    StrToVersion(FileVersionText, FileVersion) and (ComparePackedVersion(FileVersion, Existing) > 0) then begin
    InstalledVersion := FileVersionText;
    Existing := FileVersion;
  end;
  if InstalledVersion = '' then Exit;
  if not StrToVersion('{#AppVersion}', Target) then begin
    Result := False;
    Exit;
  end;
  VersionOrder := ComparePackedVersion(Existing, Target);
  if VersionOrder > 0 then begin
    SuppressibleMsgBox(
      Local('Уже установлена более новая версия GrandHonor ' + InstalledVersion + ' (текущий установщик: {#AppVersion}).',
            'A newer version of GrandHonor ' + InstalledVersion + ' is already installed (installer: {#AppVersion}).'),
      mbInformation, MB_OK, IDOK);
    Result := False;
  end;
end;

function SafeInstallPath(const Path: String): Boolean;
var
  Attributes: LongWord;
begin
  Attributes := GetFileAttributes(Path);
  Result := (Attributes = $FFFFFFFF) or ((Attributes and $400) = 0);
end;

procedure InitializeWizard;
begin
  LoadInter;
  ApplyType(WizardForm.Font, WizardForm.WelcomeLabel1.Font, WizardForm.PageNameLabel.Font);
  if InterLoaded then begin
    WizardForm.WelcomeLabel2.Font.Name := 'Inter';
    WizardForm.PageDescriptionLabel.Font.Name := 'Inter';
    WizardForm.FinishedHeadingLabel.Font.Name := 'Inter Display SemiBold';
    WizardForm.FinishedHeadingLabel.Font.Style := WizardForm.FinishedHeadingLabel.Font.Style - [fsBold];
    WizardForm.FinishedLabel.Font.Name := 'Inter';
  end;
  WizardForm.Caption := Local('Установка GrandHonor', 'GrandHonor Setup');
  WizardForm.PageNameLabel.Font.Color := WindowsAccent(WizardForm.PageNameLabel.Font.Color);
  WizardForm.WelcomeLabel1.Font.Color := WindowsAccent(WizardForm.WelcomeLabel1.Font.Color);

  if (InstalledVersion <> '') and (VersionOrder < 0) then begin
    WizardForm.WelcomeLabel1.Caption := Local('Обновление GrandHonor', 'Update GrandHonor');
    WizardForm.WelcomeLabel2.Caption := Local(
      'Обнаружена установленная версия ' + InstalledVersion + '.'#13#10#13#10 +
      'Мастер обновит файлы в Program Files\GrandHonor до версии {#AppVersion} с сохранением всех настроек.',
      'Installed version ' + InstalledVersion + ' detected.'#13#10#13#10 +
      'Setup will update GrandHonor in Program Files\GrandHonor to {#AppVersion} while keeping your settings.');
  end;

  OperationPage := CreateInputOptionPage(wpWelcome,
    Local('Обслуживание GrandHonor', 'GrandHonor Maintenance'),
    Local('Установлена версия ' + InstalledVersion, 'Installed version: ' + InstalledVersion),
    Local('GrandHonor уже установлен в Program Files\GrandHonor. Выберите нужное действие:',
          'GrandHonor is already installed in Program Files\GrandHonor. Choose an action:'),
    True, False);
  OperationPage.Add(Local('Восстановить отсутствующие или повреждённые файлы',
                          'Repair missing or damaged files'));
  OperationPage.Add(Local('Переустановить приложение целиком',
                          'Reinstall all application files'));
  OperationPage.Add(Local('Удалить GrandHonor с компьютера',
                          'Uninstall GrandHonor from this PC'));
  OperationPage.SelectedValueIndex := 0;
  if WizardSilent and (CompareText(ExpandConstant('{param:maintenance|repair}'), 'reinstall') = 0) then
    OperationPage.SelectedValueIndex := 1;
end;

function ShouldSkipPage(PageID: Integer): Boolean;
begin
  if VersionOrder = 0 then
    Result := (PageID = wpWelcome)
  else
    Result := (PageID = OperationPage.ID);
end;

function IsRepair: Boolean;
begin
  Result := (VersionOrder = 0) and (OperationPage.SelectedValueIndex = 0);
end;

{ After "Roll back", Program Files holds the version kept in the protected rollback folder,
  not this setup's build: that copy is trusted for the privileged clean-up too. }
function MatchesRollbackCopy: Boolean;
var
  Kept: String;
begin
  Result := False;
  Kept := ExpandConstant('{commonappdata}\GrandHonor\rollback\GrandHonor.exe');
  if not FileExists(Kept) or not FileExists(FixedRoot + '\GrandHonor.exe') then Exit;
  try
    Result := CompareText(GetSHA256OfFile(Kept), GetSHA256OfFile(FixedRoot + '\GrandHonor.exe')) = 0;
  except
    Log(GetExceptionMessage);
  end;
end;

function Matches(const Name, Expected: String): Boolean;
var
  Path: String;
begin
  Path := FixedRoot + '\' + Name;
  Result := False;
  if not SafeInstallPath(FixedRoot) or not SafeInstallPath(Path) or not FileExists(Path) then Exit;
  try
    Result := CompareText(GetSHA256OfFile(Path), Expected) = 0;
  except
    Log(GetExceptionMessage);
  end;
end;

function CopyHost: Boolean;
begin
  Result := not IsRepair or not Matches('GrandHonor.exe', '{#HostHash}');
end;

function CopyPanel: Boolean;
begin
  Result := not IsRepair or not Matches('GrandHonor.Panel.exe', '{#PanelHash}');
end;

function PrepareToInstall(var NeedsRestart: Boolean): String;
begin
  Result := '';
  if (CompareText(ExpandConstant('{app}'), FixedRoot) <> 0) or not SafeInstallPath(FixedRoot) or
    not SafeInstallPath(FixedRoot + '\GrandHonor.exe') or not SafeInstallPath(FixedRoot + '\GrandHonor.Panel.exe') then begin
    Result := Local('Доступна только защищённая папка Program Files\GrandHonor.',
      'Only the protected Program Files\GrandHonor directory is supported.');
    Exit;
  end;
  CloseRunningHost;
end;

function UpdateReadyMemo(Space, NewLine, UserInfo, DirInfo, TypeInfo, ComponentsInfo, GroupInfo, TasksInfo: String): String;
begin
  if InstalledVersion = '' then
    Result := Local('Установка версии ', 'Installing version ') + '{#AppVersion}'
  else if VersionOrder < 0 then
    Result := Local('Обновление ', 'Updating ') + InstalledVersion + ' → ' + '{#AppVersion}'
  else if IsRepair then
    Result := Local('Восстановление версии ', 'Repairing version ') + '{#AppVersion}'
  else
    Result := Local('Переустановка версии ', 'Reinstalling version ') + '{#AppVersion}';
  Result := Result + NewLine + NewLine + FixedRoot;
end;

function NextButtonClick(PageID: Integer): Boolean;
var
  Code, Quote: Integer;
  Uninstaller, Command, Name: String;
begin
  Result := True;
  if (PageID <> OperationPage.ID) or (OperationPage.SelectedValueIndex <> 2) then Exit;
  Result := False;
  Uninstaller := FixedRoot + '\unins000.exe';
  if RegQueryStringValue(HKLM64, AppReg, 'UninstallString', Command) and (Copy(Command, 1, 1) = '"') then begin
    Delete(Command, 1, 1);
    Quote := Pos('"', Command);
    if Quote > 0 then Uninstaller := Copy(Command, 1, Quote - 1);
  end;
  Name := Lowercase(ExtractFileName(Uninstaller));
  if (CompareText(ExtractFileDir(Uninstaller), FixedRoot) <> 0) or not SafeInstallPath(FixedRoot)
    or not SafeInstallPath(Uninstaller) or (Length(Name) <> 12) or (Copy(Name, 1, 5) <> 'unins')
    or (StrToIntDef(Copy(Name, 6, 3), -1) < 0) or (Copy(Name, 9, 4) <> '.exe')
    or not FileExists(Uninstaller) then begin
    MsgBox(Local('Деинсталлятор отсутствует. Выполните переустановку.',
      'The uninstaller is missing. Reinstall GrandHonor first.'), mbError, MB_OK);
    Exit;
  end;
  if Exec(Uninstaller, '', FixedRoot, SW_SHOWNORMAL, ewWaitUntilTerminated, Code) then begin
    if Code = 0 then begin
      Removed := True;
      WizardForm.Close;
    end;
  end else begin
    MsgBox(Local('Не удалось завершить удаление GrandHonor.',
      'GrandHonor could not complete removal.'), mbError, MB_OK);
  end;
end;

procedure CancelButtonClick(PageID: Integer; var Cancel, Confirm: Boolean);
begin
  if Removed then Confirm := False;
end;

procedure ShowUninstallCompleteDialog;
var
  Form: TSetupForm;
  Logo: TBitmapImage;
  TitleLbl, BodyLbl: TNewStaticText;
  WipeBox: TNewCheckBox;
  DoneBtn: TNewButton;
  DataDir: String;
begin
  Form := CreateCustomForm(420, 208, False, True);
  try
    Form.Caption := Local('Деинсталлятор GrandHonor', 'GrandHonor Uninstaller');

    Logo := TBitmapImage.Create(Form);
    Logo.Parent := Form;
    Logo.Left := ScaleX(22);
    Logo.Top := ScaleY(20);
    Logo.Width := ScaleX(48);
    Logo.Height := ScaleY(48);
    Logo.Stretch := True;
    if UninstallLogo <> nil then Logo.Bitmap := UninstallLogo;

    TitleLbl := TNewStaticText.Create(Form);
    TitleLbl.Parent := Form;
    TitleLbl.Left := ScaleX(84);
    TitleLbl.Top := ScaleY(20);
    TitleLbl.Width := Form.ClientWidth - ScaleX(104);
    { The uninstaller has no access to setup's Inter copy: Windows 11's own display face. }
    TitleLbl.Font.Name := 'Segoe UI Variable Display Semibold';
    TitleLbl.Font.Size := 14;
    TitleLbl.Font.Style := [];
    TitleLbl.Font.Color := WindowsAccent(TitleLbl.Font.Color);
    TitleLbl.Caption := Local('Удаление завершено', 'Removal complete');

    BodyLbl := TNewStaticText.Create(Form);
    BodyLbl.Parent := Form;
    BodyLbl.Left := ScaleX(84);
    BodyLbl.Top := ScaleY(50);
    BodyLbl.Width := Form.ClientWidth - ScaleX(104);
    BodyLbl.Height := ScaleY(36);
    BodyLbl.WordWrap := True;
    BodyLbl.Font.Name := 'Segoe UI Variable Text';
    BodyLbl.Caption := Local(
      'Файлы приложения удалены. Вы можете также очистить данные GrandHonor для текущего пользователя.',
      'Application files were removed. You can also clear GrandHonor data for the current user.');

    WipeBox := TNewCheckBox.Create(Form);
    WipeBox.Parent := Form;
    WipeBox.Left := ScaleX(22);
    WipeBox.Top := ScaleY(112);
    WipeBox.Width := Form.ClientWidth - ScaleX(44);
    WipeBox.Height := ScaleY(22);
    WipeBox.Checked := False;
    WipeBox.Caption := Local(
      'Также удалить сохранённые настройки и журнал работы',
      'Also remove saved user preferences and logs');

    DoneBtn := TNewButton.Create(Form);
    DoneBtn.Parent := Form;
    DoneBtn.Caption := Local('Готово', 'Finish');
    DoneBtn.Width := Form.CalculateButtonWidth([DoneBtn.Caption]) + ScaleX(24);
    DoneBtn.Height := ScaleY(30);
    DoneBtn.Left := Form.ClientWidth - DoneBtn.Width - ScaleX(22);
    DoneBtn.Top := Form.ClientHeight - DoneBtn.Height - ScaleY(18);
    DoneBtn.ModalResult := mrOk;
    DoneBtn.Default := True;

    Form.FlipAndCenterIfNeeded(True, nil, False);
    if (Form.ShowModal = mrOk) and WipeBox.Checked then begin
      RegDeleteKeyIncludingSubkeys(HKCU, 'Software\GrandHonor');
      DataDir := ExpandConstant('{localappdata}\GrandHonor');
      if SafeInstallPath(DataDir) then begin
        DeleteFile(DataDir + '\GrandHonor.log');
        DeleteFile(DataDir + '\GrandHonor.log.1');
        DeleteFile(DataDir + '\settings-backup.json');
        RemoveDir(DataDir);
      end;
      DataDir := ExpandConstant('{userappdata}\GrandHonor');
      if SafeInstallPath(DataDir) then begin
        DeleteFile(DataDir + '\settings.json');
        DeleteFile(DataDir + '\settings.json.tmp');
        DeleteFile(DataDir + '\settings.json.broken');
        RemoveDir(DataDir);
      end;
    end;
  finally
    Form.Free;
  end;
end;

function InitializeUninstall: Boolean;
begin
  Result := True;
end;

procedure InitializeUninstallProgressForm;
begin
  UninstallLogo := TBitmap.Create;
  UninstallLogo.Assign(UninstallProgressForm.WizardSmallBitmapImage.Bitmap);
  UninstallProgressForm.Caption := Local('Деинсталлятор GrandHonor {#AppVersion}', 'GrandHonor Uninstaller {#AppVersion}');
  UninstallProgressForm.PageNameLabel.Font.Color := WindowsAccent(UninstallProgressForm.PageNameLabel.Font.Color);
  UninstallProgressForm.PageNameLabel.Font.Name := 'Segoe UI Variable Display Semibold';
  UninstallProgressForm.PageNameLabel.Font.Style := [];
  UninstallProgressForm.PageNameLabel.Caption := Local('Удаление GrandHonor', 'Uninstalling GrandHonor');
  UninstallProgressForm.PageDescriptionLabel.Caption :=
    Local('Очистка файлов в Program Files\GrandHonor и задачи автозапуска...',
          'Removing files from Program Files\GrandHonor and startup task...');
end;

{ Explorer keeps a tray-icon entry per program in HKCU\Control Panel\NotifyIconSettings (GrandHonor
  marks it IsPromoted on first run). Removes the entries for this installation's GrandHonor.exe,
  stored either as a plain path or with the Program Files known-folder GUID. }
procedure RemoveTrayIconEntries;
var
  Keys: TArrayOfString;
  I: Integer;
  Path, Plain, Known, Pf: String;
begin
  Plain := Lowercase(FixedRoot + '\GrandHonor.exe');
  Known := '';
  Pf := ExpandConstant('{commonpf64}');
  if CompareText(Copy(FixedRoot, 1, Length(Pf)), Pf) = 0 then
    Known := Lowercase('{6D809377-6AF0-444B-8957-A3773F02200E}' + Copy(FixedRoot, Length(Pf) + 1, MaxInt) + '\GrandHonor.exe');
  if not RegGetSubkeyNames(HKCU, 'Control Panel\NotifyIconSettings', Keys) then Exit;
  for I := 0 to GetArrayLength(Keys) - 1 do
    if RegQueryStringValue(HKCU, 'Control Panel\NotifyIconSettings\' + Keys[I], 'ExecutablePath', Path) then begin
      Path := Lowercase(Path);
      if (Path = Plain) or ((Known <> '') and (Path = Known)) then
        RegDeleteKeyIncludingSubkeys(HKCU, 'Control Panel\NotifyIconSettings\' + Keys[I]);
    end;
end;

procedure CurUninstallStepChanged(Step: TUninstallStep);
var
  Code: Integer;
  Startup: String;
begin
  if Step = usUninstall then begin
    CloseRunningHost;
    if Matches('GrandHonor.exe', '{#HostHash}') or MatchesRollbackCopy then begin
      if not Exec(FixedRoot + '\GrandHonor.exe', '--uninstall-privileged-tasks', FixedRoot,
        SW_HIDE, ewWaitUntilTerminated, Code) or (Code <> 0) then
        Log('Could not remove the GrandHonor startup task.');
    end else
      Log('Skipping startup cleanup because GrandHonor.exe is missing or its hash differs.');
    if RegQueryStringValue(HKCU, 'Software\Microsoft\Windows\CurrentVersion\Run', 'GrandHonor', Startup)
      and (CompareText(Startup, '"' + FixedRoot + '\GrandHonor.exe" --background') = 0) then
      RegDeleteValue(HKCU, 'Software\Microsoft\Windows\CurrentVersion\Run', 'GrandHonor');
    RemoveTrayIconEntries;
    { The previous version kept for rollback belongs to this installation. }
    DelTree(ExpandConstant('{commonappdata}\GrandHonor\rollback'), True, True, True);
    RemoveDir(ExpandConstant('{commonappdata}\GrandHonor'));
  end else if (Step = usPostUninstall) and not UninstallSilent then begin
    ShowUninstallCompleteDialog;
  end;
end;
