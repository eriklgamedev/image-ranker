; Required native ABI/plugin input. No alternate plugin path or unguarded mode.
!addplugindir /x86-unicode "${__FILEDIR__}\..\nsis-plugin\artifacts"
!include "${__FILEDIR__}\image-ranker-utils.nsh"

!macro ImageRankerRequireSuccess
  ClearErrors
  Pop $R9
  ${If} ${Errors}
    SetErrorLevel 73
    Abort "The native installer guard did not return a result. No further changes will be made."
  ${EndIf}
  ${If} $R9 != "OK"
    SetErrorLevel 73
    DetailPrint "$R9"
    ; Passive/silent admission also fails without a modal prompt. The caller's
    ; installer log and exit status carry the exact native reason.
    Abort
  ${EndIf}
!macroend

!macro ImageRankerValidate
  ImageRankerLifetime::ValidateHeld
  !insertmacro ImageRankerRequireSuccess
!macroend
