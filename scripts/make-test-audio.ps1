# Creates 16 kHz mono PCM16 WAV samples with Windows speech synthesis for offline and
# end-to-end tests. Output goes to target\test-audio (ignored by Git).
param(
    [string]$OutDir = (Join-Path $PSScriptRoot '..\target\test-audio')
)

$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.Speech
New-Item -ItemType Directory -Force $OutDir | Out-Null

$samples = @{
    'zh-short.wav' = '今天下午三点开会，请大家准时参加。'
    'zh-long.wav'  = '实时聆听可以把说话内容写进当前的文本框。按住右侧控制键说话，松开就会结束并写入最后一句。'
}

$format = New-Object System.Speech.AudioFormat.SpeechAudioFormatInfo(16000, [System.Speech.AudioFormat.AudioBitsPerSample]::Sixteen, [System.Speech.AudioFormat.AudioChannel]::Mono)
foreach ($name in $samples.Keys) {
    $synth = New-Object System.Speech.Synthesis.SpeechSynthesizer
    try {
        $voice = $synth.GetInstalledVoices() | Where-Object { $_.Enabled -and $_.VoiceInfo.Culture.Name -eq 'zh-CN' } | Select-Object -First 1
        if (-not $voice) { throw 'No enabled zh-CN voice is installed.' }
        $synth.SelectVoice($voice.VoiceInfo.Name)
        $path = Join-Path $OutDir $name
        $synth.SetOutputToWaveFile($path, $format)
        $synth.Speak($samples[$name])
    } finally {
        $synth.Dispose()
    }
    Write-Output "$path"
}
