# 随手记原生控件检查

此工程只读复制已构建的 `LiveListen.dll`，在独立 WinUI 进程中通过反射调用产品 `NoteEditor` 的真实实现。它不运行产品入口、语音核心或凭据逻辑，不激活窗口，不发送鼠标键盘输入，不读取用户卡片。

先构建产品 Debug 版本，再执行：

```powershell
dotnet build app/LiveListen.EditorChecks/LiveListen.EditorChecks.csproj -c Debug -p:Platform=x64
$check = Start-Process -FilePath .\target\editor-checks\bin-sdk18\LiveListen.EditorChecks.exe -WindowStyle Hidden -PassThru -Wait
$check.ExitCode
Get-Content .\target\editor-checks\report.json
```

默认使用产品 Debug DLL，可用 `-p:LiveListenAssembly=绝对路径` 指定其他已有构建。测试与产品使用相同的 Windows App SDK 1.8 组件；测试结果、产品 DLL SHA-256 和原生 WinUI DLL 版本/哈希写入 `target/editor-checks/report.json`。测试覆盖格式保存重开、段落列表、纯文本对应、读写状态、编辑工具栏及长度限制；隐藏窗口不能代替悬停、弹出菜单位置、链接弹层实际键盘输入和真实鼠标点击的视觉验收。

SDK 切换使用独立输出目录 `bin-sdk18`，避免自包含输出残留其他 SDK 的原生 DLL。可用 `-p:EditorChecksOutputPath=绝对目录` 指定其他全新输出目录；实际验收时应核对报告中的原生 WinUI 哈希与产品目录一致。

隐藏窗口不会派发完整的选区和焦点路由事件，因此这部分回归直接调用产品的实际事件处理函数，并使用原生文档状态验证结果。检查覆盖同文档加载保留选区与撤销历史、系统选区浮动栏禁用、菜单打开期间保护编辑状态、三种列表矢量图标和清除列表命令。原生保存的柔白、柔黑可恢复为自动字色。切换列表类型时，正文中的字面复选框符号仍是普通文字。

链接回归调用产品实际添加/修改/移除流程，覆盖有选区、无选区、光标位于已有链接、无显示文字时使用地址、非法地址无损拒绝，以及选中首行（含或不含段落符）添加链接后不吞掉后续换行。表单检查两个输入框可编辑及焦点属性；弹层打开后的真实输入和关闭后的正文键盘焦点需另外做可见验收。

连续重开回归检查空文档、内部空行、显式末尾空行，以及含加粗、颜色、链接和编号的文档经过五轮保存重开后的完整文本与段落数量。诊断输出 `reopen-diagnostics.json`。另见 `native-reopen-diagnostics.json`，其中对比 WinUI 的 RTF 片段插入与完整文档打开行为。
