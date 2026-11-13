; Extra installer steps for Mouse without borders.
; After installing, register the program to start when the user signs in,
; so it works after a restart even if it was never opened by hand.
!macro NSIS_HOOK_POSTINSTALL
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Run" "MouseWithoutBorders" "$\"$INSTDIR\${MAINBINARYNAME}.exe$\" --autostart"
!macroend

; On uninstall, remove that start-up entry again.
!macro NSIS_HOOK_POSTUNINSTALL
  DeleteRegValue HKCU "Software\Microsoft\Windows\CurrentVersion\Run" "MouseWithoutBorders"
!macroend
