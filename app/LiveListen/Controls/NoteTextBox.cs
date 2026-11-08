using Microsoft.UI.Input;
using Microsoft.UI.Xaml.Controls;

namespace LiveListen.Controls;

/// <summary>Keeps the native text-area cursor stable without per-pointer cursor updates.</summary>
internal sealed partial class NoteTextBox : RichEditBox
{
    public NoteTextBox() => ProtectedCursor = InputSystemCursor.Create(InputSystemCursorShape.IBeam);
}
