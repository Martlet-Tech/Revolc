$clsid = 'HKLM:\SOFTWARE\Classes\CLSID\{9F1C4E8B-6A2D-4E7B-8C13-5D04F7A2B916}'
New-Item -Path (Join-Path $clsid 'InprocServer32') -Force | Out-Null
Set-Item -Path $clsid -Value 'Revolc Tab Explorer Helper'
Set-ItemProperty -Path (Join-Path $clsid 'InprocServer32') -Name '(default)' -Value 'D:\Projects\file-explorer\Revolc\target\debug\revolc_bho.dll'
Set-ItemProperty -Path (Join-Path $clsid 'InprocServer32') -Name 'ThreadingModel' -Value 'Apartment'
'DONE' | Out-File -FilePath "$env:APPDATA\revolc\hklm_clsid_done.txt" -Encoding ascii
