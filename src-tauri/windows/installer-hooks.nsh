; Extra steps for the JOJO Server installer.
; The chosen data folder (programs, databases and the 'www' projects folder) is saved to
; HKCU\Software\JOJO Server\RootDir and read by the app.

!macro NSIS_HOOK_PREINSTALL
  ReadRegStr $R0 HKCU "Software\JOJO Server" "RootDir"
  ; Already chosen (a re-install or an automatic update): keep it and do not ask again.
  ${If} $R0 == ""
  ${AndIfNot} ${Silent}
    ; First install: prefer drive D: (a fixed disk) so downloads and databases do not fill C:.
    System::Call 'kernel32::GetDriveTypeW(w "D:\")i .r1'
    ${If} $1 == 3
      StrCpy $R0 "D:\JojoServer"
    ${Else}
      StrCpy $R0 "$PROFILE\JojoServer"
    ${EndIf}

    MessageBox MB_YESNO|MB_ICONQUESTION "JOJO Server keeps its programs (PHP, MySQL...), your databases and your websites in one folder. Your websites go in its 'www' sub-folder.$\r$\n$\r$\nFolder:$\r$\n$R0$\r$\n$\r$\nDo you want to choose a different folder?" /SD IDNO IDNO jojo_root_chosen
      ; The dialog opens inside the suggested folder, so create it first.
      CreateDirectory "$R0"
      nsDialogs::SelectFolderDialog "Choose the folder for JOJO Server's files (www goes inside it)" "$R0"
      Pop $R1
      ${If} $R1 != "error"
      ${AndIf} $R1 != ""
        StrCpy $R0 $R1
      ${EndIf}
    jojo_root_chosen:

    ; A drive root such as D:\ would be too messy, so use a sub-folder.
    StrCpy $R2 $R0 "" -2
    ${If} $R2 == ":\"
      StrCpy $R0 "$R0JojoServer"
    ${EndIf}

    CreateDirectory "$R0"
    CreateDirectory "$R0\www"
    WriteRegStr HKCU "Software\JOJO Server" "RootDir" "$R0"
    DeleteRegValue HKCU "Software\JOJO Server" "WwwDir"
  ${EndIf}
!macroend

!macro NSIS_HOOK_POSTUNINSTALL
  ; Nothing is deleted from the data folder, and the chosen folder is remembered in the registry so a
  ; later re-install (or an automatic update that replaces the old version) finds your data again.
!macroend
