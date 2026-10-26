; Lumen's NSIS installer hooks, wired through bundle.windows.nsis.installerHooks.
;
; After an install, two kinds of registry entry an install made under Lumen's previous identifier
; can leave are removed. Both are installer metadata, never user data, and both are found by what
; they hold, never by a name: this file spells no vendor and no identifier.
;
; 1. A second entry in Apps & features: a subkey of HKCU\...\Uninstall other than "Lumen" whose
;    DisplayName is "Lumen" and whose InstallLocation is this $INSTDIR. Without this, Lumen is
;    listed twice and the stale entry's uninstaller removes the live install.
; 2. The previous manufacturer key: HKCU\Software\<vendor>\Lumen whose default value is this
;    $INSTDIR, under any vendor but this build's ${MANUFACTURER}. The bundler derived the old
;    manufacturer from the identifier.
;
; The registers used are saved and restored: the hook runs inside the template's install section.

!macro NSIS_HOOK_POSTINSTALL
  Push $R0
  Push $R1
  Push $R2

  ; 1. Duplicate uninstall entries. A deleted subkey moves the ones after it up, so the index
  ;    stays where it is after a delete.
  StrCpy $R0 0
  lumen_uninstall_loop:
    EnumRegKey $R1 HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall" $R0
    StrCmp $R1 "" lumen_uninstall_done
    StrCmp $R1 "${PRODUCTNAME}" lumen_uninstall_next
    ReadRegStr $R2 HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\$R1" "DisplayName"
    StrCmp $R2 "${PRODUCTNAME}" 0 lumen_uninstall_next
    ReadRegStr $R2 HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\$R1" "InstallLocation"
    ; The template writes the location quoted; an older bundler wrote it bare.
    StrCmp $R2 "$\"$INSTDIR$\"" lumen_uninstall_drop
    StrCmp $R2 "$INSTDIR" lumen_uninstall_drop lumen_uninstall_next
  lumen_uninstall_drop:
    DetailPrint "Removing a second Lumen entry in Apps & features ($R1)"
    ClearErrors
    DeleteRegKey HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\$R1"
    ; A key that could not be deleted is stepped over, or the loop would meet it forever.
    IfErrors lumen_uninstall_next lumen_uninstall_loop
  lumen_uninstall_next:
    IntOp $R0 $R0 + 1
    Goto lumen_uninstall_loop
  lumen_uninstall_done:

  ; 2. The previous manufacturer key. When the vendor key is left empty it goes too, and the index
  ;    stays; when it holds anything else it stays, and the next pass finds no Lumen key under it.
  StrCpy $R0 0
  lumen_vendor_loop:
    EnumRegKey $R1 HKCU "Software" $R0
    StrCmp $R1 "" lumen_vendor_done
    StrCmp $R1 "${MANUFACTURER}" lumen_vendor_next
    ClearErrors
    ReadRegStr $R2 HKCU "Software\$R1\${PRODUCTNAME}" ""
    IfErrors lumen_vendor_next
    StrCmp $R2 "$INSTDIR" 0 lumen_vendor_next
    DetailPrint "Removing the previous install-location key (Software\$R1\${PRODUCTNAME})"
    ClearErrors
    DeleteRegKey HKCU "Software\$R1\${PRODUCTNAME}"
    IfErrors lumen_vendor_next
    DeleteRegKey /ifempty HKCU "Software\$R1"
    Goto lumen_vendor_loop
  lumen_vendor_next:
    IntOp $R0 $R0 + 1
    Goto lumen_vendor_loop
  lumen_vendor_done:

  Pop $R2
  Pop $R1
  Pop $R0
!macroend
