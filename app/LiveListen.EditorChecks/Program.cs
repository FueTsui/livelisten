using System.Reflection;
using System.Diagnostics;
using System.Security.Cryptography;
using System.Text.Json;
using System.Text.Json.Nodes;
using System.Text.RegularExpressions;
using Microsoft.UI.Dispatching;
using Microsoft.UI.Text;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Media;
using Windows.Foundation;
using Windows.UI;

namespace LiveListen.EditorChecks;

internal static class Program
{
    internal static string ReportPath = "";
    internal static int ResultCode = 1;
    internal static void Phase(string value)
    {
        Directory.CreateDirectory(Path.GetDirectoryName(ReportPath)!);
        File.WriteAllText(Path.ChangeExtension(ReportPath, ".phase.txt"), value);
    }

    [STAThread]
    private static int Main(string[] args)
    {
        ReportPath = args.Length > 0 ? Path.GetFullPath(args[0]) : Path.GetFullPath(Path.Combine(AppContext.BaseDirectory, "..", "report.json"));
        try
        {
            Phase("Main: initialize COM wrappers");
            WinRT.ComWrappersSupport.InitializeComWrappers();
            Phase("Main: Application.Start");
            Application.Start(parameters =>
            {
                Phase("Application.Start callback");
                SynchronizationContext.SetSynchronizationContext(new DispatcherQueueSynchronizationContext(DispatcherQueue.GetForCurrentThread()));
                _ = new ChecksApp();
            });
        }
        catch (Exception e)
        {
            Directory.CreateDirectory(Path.GetDirectoryName(ReportPath)!);
            File.WriteAllText(ReportPath, JsonSerializer.Serialize(new { fatal = e.ToString() }, new JsonSerializerOptions { WriteIndented = true }));
        }
        return ResultCode;
    }
}

public sealed partial class ChecksApp : Application
{
    private readonly List<object> checks = [];
    private readonly StackPanel host;
    private Window? window;
    private Assembly product = null!;
    private Type editorType = null!;
    private int failed;
    private bool finishing;

    public ChecksApp()
    {
        Program.Phase("ChecksApp constructor");
        InitializeComponent();
        Program.Phase("ChecksApp initialized resources");
        DispatcherShutdownMode = DispatcherShutdownMode.OnExplicitShutdown;
        host = new StackPanel { Width = 240 };
        UnhandledException += (_, e) =>
        {
            e.Handled = true;
            failed++;
            checks.Add(new { name = "unhandled", passed = false, detail = e.Exception.ToString() });
            Finish();
        };
    }

    protected override void OnLaunched(LaunchActivatedEventArgs args)
    {
        Program.Phase("OnLaunched: hidden Window");
        window = new Window { Title = "LiveListen editor checks (hidden)", Content = host };
        // Never call Window.Activate, AppWindow.Show or any input/foreground API.
        window.AppWindow.Hide();
        DispatcherQueue.GetForCurrentThread().TryEnqueue(Run);
    }

    private void Check(string name, Func<string> action)
    {
        try { checks.Add(new { name, passed = true, detail = action() }); }
        catch (Exception e)
        {
            failed++;
            while (e is TargetInvocationException { InnerException: { } inner }) e = inner;
            checks.Add(new { name, passed = false, detail = e.ToString() });
        }
    }

    private void Run()
    {
        Program.Phase("Run checks");
        try
        {
            product = Assembly.LoadFrom(Path.Combine(AppContext.BaseDirectory, "LiveListen.dll"));
            editorType = product.GetType("LiveListen.Controls.NoteEditor", throwOnError: true)!;
            product.GetType("LiveListen.Services.Loc", true)!.GetMethod("Init")!.Invoke(null, ["zh-Hans"]);

            Check("initial-load-and-normalization", () =>
            {
                var note = NewNote("你好\n第二行");
                Equal("你好\n第二行", Content(note).Text);
                Require(!Native(note).IsReadOnly, "Idle notes must remain editable without a read-only transition on selection.");
                Require(Field<Border>(note, "toolbar").Visibility == Visibility.Collapsed, "Toolbar is visible before editing.");
                Require(!Content(note).Rtf.Contains('\0'), "RTF contains NUL.");
                return "Native RichEditBox loaded normalized text and clean RTF; the toolbar remains hidden until interaction.";
            });

            Check("toolbar-enter-exit-and-selection-formatting", () =>
            {
                var note = NewNote("alpha beta");
                Call(note, "BeginEditing");
                Require(Get<bool>(note, "IsEditing") && !Native(note).IsReadOnly, "BeginEditing did not enable editing.");
                Require(Field<Border>(note, "toolbar").Visibility == Visibility.Visible, "Editing toolbar was not shown.");
                Native(note).Document.Selection.SetRange(0, 5);
                Call(note, "Character", (Action<ITextCharacterFormat>)(f => f.Bold = FormatEffect.On));
                Call(note, "Character", (Action<ITextCharacterFormat>)(f => f.Italic = FormatEffect.On));
                Call(note, "Character", (Action<ITextCharacterFormat>)(f => f.Underline = UnderlineType.Single));
                Call(note, "Character", (Action<ITextCharacterFormat>)(f => f.Strikethrough = FormatEffect.On));
                Require(Native(note).Document.Selection.StartPosition == 0 && Native(note).Document.Selection.EndPosition == 5, "Formatting moved the selected range.");
                Require(Native(note).Document.GetRange(6, 10).CharacterFormat.Bold != FormatEffect.On, "Formatting leaked outside the selected span.");
                var saved = Content(note);
                var reopened = NewNote(saved.Text, saved.Rtf);
                var f = Native(reopened).Document.GetRange(0, 5).CharacterFormat;
                Require(f.Bold == FormatEffect.On && f.Italic == FormatEffect.On && f.Underline == UnderlineType.Single && f.Strikethrough == FormatEffect.On, "Native RTF did not retain all four span formats.");
                Call(note, "FinishEditing");
                Require(!Get<bool>(note, "IsEditing") && !Native(note).IsReadOnly, "FinishEditing must hide the toolbar without flipping document read-only state.");
                Require(Field<Border>(note, "toolbar").Visibility == Visibility.Collapsed, "Toolbar did not hide after editing.");
                return "B/I/U/S were applied to a selected span, retained on reload, and the editing toolbar entered/exited correctly.";
            });

            Check("focus-gated-activation-and-background-notification-inert", () =>
            {
                var note = NewNote("alpha beta");
                var doc = Native(note).Document;
                doc.Selection.SetRange(0, 5);
                // Unshown XAML roots do not dispatch native selection routed events;
                // exercise the real event handler over the real native selection.
                Call(note, "EditorSelectionChanged");
                Require(!Get<bool>(note, "IsEditing"), "An unfocused programmatic selection incorrectly opened the toolbar.");
                Call(note, "EditorGotFocus", Native(note), new RoutedEventArgs());
                Require(Get<bool>(note, "IsEditing"), "The focus handler did not reveal the toolbar.");
                Call(note, "FinishEditing");
                doc.Selection.SetRange(0, 5);
                Call(note, "EditorSelectionChanged");
                Require(!Get<bool>(note, "IsEditing"), "Replaying an unchanged selection restarted the editing session.");
                Call(note, "EditorGotFocus", Native(note), new RoutedEventArgs());
                Require(Get<bool>(note, "IsEditing"), "The actual focus handler did not reveal the toolbar.");
                Equal(0, doc.Selection.StartPosition);
                Equal(5, doc.Selection.EndPosition);
                Call(note, "FinishEditing");
                doc.Selection.SetRange(doc.Selection.StoryLength - 1, doc.Selection.StoryLength - 1);
                doc.Selection.TypeText("!");
                Call(note, "EditorTextChanged");
                Require(!Get<bool>(note, "IsEditing"), "An unfocused delayed text notification reopened the toolbar.");
                Equal("alpha beta!", Content(note).Text);
                Call(note, "EditorGotFocus", Native(note), new RoutedEventArgs());
                Require(Get<bool>(note, "IsEditing"), "Returning text focus did not start an editing session.");
                Call(note, "Pause");
                Require(Native(note).IsReadOnly, "Pause must still protect the document during shutdown.");
                Call(note, "EditorGotFocus", Native(note), new RoutedEventArgs());
                Require(!Get<bool>(note, "IsEditing"), "Focus reentry reopened editing while paused.");
                Call(note, "Resume");
                Require(!Native(note).IsReadOnly, "Resume did not restore editing.");
                return "The focus handler starts editing; unfocused selection/text notifications, repeated selection and paused focus are inert. Actual routed events and typing-after-Esc are checked separately in a visible window.";
            });

            Check("same-document-load-preserves-selection-and-undo", () =>
            {
                var note = NewNote("alpha beta");
                var doc = Native(note).Document;
                doc.Selection.SetRange(2, 7);
                Call(note, "FinishEditing");
                var selectionEvents = 0;
                Native(note).SelectionChanged += (_, _) => selectionEvents++;
                Call(note, "Load", "alpha beta", null);
                Equal(2, doc.Selection.StartPosition);
                Equal(7, doc.Selection.EndPosition);
                Equal(0, selectionEvents);
                Require(!Get<bool>(note, "IsEditing"), "A replayed source snapshot reopened the toolbar.");
                doc.ClearUndoRedoHistory();
                doc.Selection.SetRange(10, 10);
                doc.Selection.TypeText("!");
                Require(doc.CanUndo(), "Native TypeText did not create an undo item.");
                doc.Selection.SetRange(1, 5);
                var snapshot = Content(note);
                Call(note, "Load", snapshot.Text, snapshot.Rtf);
                Equal(1, doc.Selection.StartPosition);
                Equal(5, doc.Selection.EndPosition);
                Require(doc.CanUndo(), "A duplicate saved snapshot cleared the undo history.");
                doc.Undo();
                Equal("alpha beta", Content(note).Text);
                return "Identical plain source and canonical RTF acknowledgements do not reset selection or undo; undo still reverts the original edit.";
            });

            Check("desktop-editor-waits-for-content-interaction", () =>
            {
                var note = Activator.CreateInstance(editorType, [true])!;
                Call(note, "Load", "desktop draft", null);
                var native = Native(note);
                var controls = Field<Dictionary<string, Button>>(note, "toggles");
                var toolbar = Field<Border>(note, "toolbar");
                Call(note, "EditorSelectionChanged");
                Call(note, "EditorTextChanged");
                Require(!Get<bool>(note, "IsEditing") && native.IsReadOnly, "A passive notification unlocked the desktop editor.");
                Equal(0, controls.Count);
                Equal(Visibility.Collapsed, toolbar.Visibility);
                Call(note, "BeginEditing");
                Equal(6, controls.Count);
                Require(!native.IsReadOnly && toolbar.Visibility == Visibility.Visible, "Explicit editing did not enable the toolbar.");
                Equal(0d, toolbar.BorderThickness.Top);
                Equal(1d, Field<Border>(NewNote("notebook note"), "toolbar").BorderThickness.Top);
                native.Document.SetText(TextSetOptions.None, " \r\n ");
                Require(Get<bool>(note, "IsEmpty"), "A cleared desktop note still has visible content.");
                Call(note, "FinishEditing");
                Call(note, "Resume");
                Require(native.IsReadOnly && toolbar.Visibility == Visibility.Collapsed, "Returning to idle did not lock the editor and hide its toolbar.");
                return "Desktop editors construct the toolbar on explicit edit and ignore passive content notifications.";
            });

            Check("inline-toolbar-and-disabled-selection-flyout", () =>
            {
                var note = NewNote("selected text");
                var native = Native(note);
                native.Style = (Style)Resources["DefaultRichEditBoxStyle"];
                native.ApplyTemplate();
                Require(native.SelectionFlyout is null, "System text-selection toolbar is still enabled.");
                Require(native.ContextFlyout is not null, "Native right-click text commands were removed.");
                var external = new MenuFlyout();
                Call(note, "TrackExternalFlyout", external, false);
                Call(note, "TrackExternalFlyout", external, false);
                Equal(1, Field<HashSet<Microsoft.UI.Xaml.Controls.Primitives.FlyoutBase>>(note, "trackedExternalFlyouts").Count);
                Require(!Get<bool>(note, "HasOpenFlyout"), "Registering a closed external menu marked it as open.");
                Call(note, "TrackExternalFlyout", external, true);
                Require(Get<bool>(note, "HasOpenFlyout"), "Attaching an active editor from an external menu Opening event did not protect it.");
                Equal(1, Field<HashSet<Microsoft.UI.Xaml.Controls.Primitives.FlyoutBase>>(note, "trackedExternalFlyouts").Count);
                var borrowed = new MenuFlyout();
                Call(note, "TrackExternalFlyout", borrowed, true);
                Call(note, "ExternalFlyoutClosed", borrowed, new object());
                Require(!Field<HashSet<Microsoft.UI.Xaml.Controls.Primitives.FlyoutBase>>(note, "trackedExternalFlyouts").Contains(borrowed),
                    "A closed menu owned by another card remained retained.");
                var toolbar = Field<Border>(note, "toolbar");
                Equal(new CornerRadius(0), toolbar.CornerRadius);
                Equal(new Thickness(0, 1, 0, 0), toolbar.BorderThickness);
                Require(toolbar.Background is SolidColorBrush { Color.A: 0 }, "Inline toolbar has an opaque floating pill background.");
                Equal(1, Grid.GetRow(toolbar));
                Require(Get<Grid>(note, "View").Children.Contains(toolbar), "Toolbar is not in the note's normal layout.");
                return "SelectionFlyout is disabled, native context commands remain, and the transparent toolbar is in row 1 with only a top separator.";
            });

            Check("card-theme-default-color-and-custom-span-preservation", () =>
            {
                var cardLook = product.GetType("LiveListen.Services.Theme", true)!.GetMethod("CardLook", BindingFlags.Static | BindingFlags.Public | BindingFlags.NonPublic)!;
                object Look(string theme, JsonObject style) => cardLook.Invoke(null, [theme, (Func<string, JsonNode?>)(key => style[key])])!;
                var light = Look("dark", new JsonObject { ["background"] = "#FFF8F8F8", ["text"] = "" });
                var dark = Look("light", new JsonObject { ["background"] = "#FF202020", ["text"] = "" });
                Equal(Color.FromArgb(255, 36, 36, 36), Get<Color>(light, "Text"));
                Equal(Color.FromArgb(255, 245, 245, 245), Get<Color>(dark, "Text"));
                Require(!Get<bool>(light, "Dark") && Get<bool>(dark, "Dark"), "Card background did not decide the foreground tone.");
                var custom = Color.FromArgb(255, 192, 44, 214);
                var explicitLook = Look("light", new JsonObject { ["background"] = "#FF202020", ["text"] = "#FFC02CD6" });
                Equal(custom, Get<Color>(explicitLook, "Text"));
                var note = NewNote("default custom");
                var plainSnapshot = Content(note);
                var plainReopened = NewNote(plainSnapshot.Text, plainSnapshot.Rtf);
                Equal(TextConstants.AutoColor, Native(plainReopened).Document.GetRange(0, 7).CharacterFormat.ForegroundColor);
                Call(note, "BeginEditing");
                var doc = Native(note).Document;
                doc.Selection.SetRange(8, 14);
                Call(note, "Character", (Action<ITextCharacterFormat>)(f => f.ForegroundColor = custom));
                foreach (var look in new[] { dark, light })
                {
                    Call(note, "Apply", look);
                    Equal(TextConstants.AutoColor, doc.GetRange(0, 7).CharacterFormat.ForegroundColor);
                    Equal(custom, doc.GetRange(8, 14).CharacterFormat.ForegroundColor);
                    Equal(Get<Color>(look, "Text"), ((SolidColorBrush)Native(note).Foreground).Color);
                    Equal(8, doc.Selection.StartPosition);
                    Equal(14, doc.Selection.EndPosition);
                }
                return "CardLook chooses soft white/black from the card background and preserves explicit colors; applying both themes retains native AutoColor, custom span color and selection.";
            });

            Check("serialized-soft-defaults-restore-automatic-color", () =>
            {
                var note = NewNote("soft light", @"{\rtf1\ansi{\colortbl;\red36\green36\blue36;\red245\green245\blue245;}\cf1\b soft \cf2\i light}");
                var document = Native(note).Document;
                Equal(TextConstants.AutoColor, document.GetRange(0, 4).CharacterFormat.ForegroundColor);
                Equal(TextConstants.AutoColor, document.GetRange(5, 10).CharacterFormat.ForegroundColor);
                Equal(FormatEffect.On, document.GetRange(0, 4).CharacterFormat.Bold);
                Equal(FormatEffect.On, document.GetRange(5, 10).CharacterFormat.Italic);
                return "Soft foreground colors emitted by RichEdit reopen as automatic color with formatting intact.";
            });

            Check("appearance-refresh-preserves-live-document", () =>
            {
                var lookType = product.GetType("LiveListen.Services.SurfaceLook", true)!;
                var textColor = Color.FromArgb(255, 245, 245, 245);
                var background = Color.FromArgb(255, 32, 32, 32);
                object Look(double padding) => Activator.CreateInstance(
                    lookType, [true, textColor, background, background, 8f, "none", padding])!;
                var note = NewNote("bold text");
                Call(note, "Apply", Look(12));
                Call(note, "BeginEditing");
                var selection = Native(note).Document.Selection;
                selection.SetRange(0, 4);
                Call(note, "Character", (Action<ITextCharacterFormat>)(format =>
                {
                    format.Bold = FormatEffect.On;
                    format.ForegroundColor = textColor;
                }));
                var before = Content(note);
                Call(note, "Apply", Look(12));
                Call(note, "Apply", Look(16));
                Equal(before, Content(note));
                Equal(0, selection.StartPosition);
                Equal(4, selection.EndPosition);
                return "Repeated appearance and padding refreshes preserve live rich text and selection.";
            });

            Check("list-menu-icons-are-font-independent-geometry", () =>
            {
                var icons = product.GetType("LiveListen.Controls.NoteListIcons", true)!;
                foreach (var kind in new[] { "bullet", "dash", "number" })
                {
                    var icon = (PathIcon)icons.GetMethod("Create")!.Invoke(null, [kind, null])!;
                    Equal(22d, icon.Width);
                    Equal(22d, icon.Height);
                    Require(icon.Data is GeometryGroup group && group.Children.Count > 3, "List menu icon was not built from geometry.");
                }
                Require(Native(NewNote("cursor")).GetType().Name == "NoteTextBox", "Text area does not use its fixed-I-beam subclass.");
                var note = NewNote("list");
                var tools = Field<Dictionary<string, Button>>(note, "toggles");
                Require(!tools.ContainsKey("color") && !tools.ContainsKey("quote"), "Removed format actions remain.");
                var menu = (MenuFlyout)Call(note, "BuildListMenu")!;
                Equal(4, menu.Items.OfType<MenuFlyoutItem>().Count());
                var before = Content(note);
                Call(note, "SetList", "todo");
                Equal(before, Content(note));
                return "Only bullet, dash, numbering and clear-list commands remain; three list icons use vectors. Removed checklist creation is inert.";
            });

            Check("numbered-list-continuity-and-roundtrip", () =>
            {
                var note = NewNote("alpha\nbeta");
                Call(note, "BeginEditing");
                SelectAll(note);
                Call(note, "SetList", "number");
                var saved = Content(note);
                var reopened = NewNote(saved.Text, saved.Rtf);
                Native(reopened).Document.GetText(TextGetOptions.NoHidden | TextGetOptions.IncludeNumbering, out var numbered);
                Require(Regex.IsMatch(numbered, @"1\..*alpha[\s\S]*2\..*beta"), "Expected 1. alpha / 2. beta; actual " + JsonSerializer.Serialize(numbered));
                Require(Native(reopened).Document.GetRange(0, 5).ParagraphFormat.ListType == MarkerType.Arabic, "Number list format was lost.");
                Equal("alpha\nbeta", saved.Text);
                return "Two paragraphs export as 1 and 2 after native RTF reload.";
            });

            Check("dash-conversion-preserves-literal-symbols-and-previous-paragraph", () =>
            {
                var note = NewNote("previous\n☐ alpha\n☑ beta");
                Call(note, "BeginEditing");
                Native(note).Document.Selection.SetRange(9, Native(note).Document.Selection.StoryLength - 1);
                var saved = Content(note);
                var reopened = NewNote(saved.Text, saved.Rtf);
                Equal(saved.Text, Content(reopened).Text);
                Call(reopened, "BeginEditing");
                Native(reopened).Document.Selection.SetRange(9, Native(reopened).Document.Selection.StoryLength - 1);
                Call(reopened, "SetList", "dash");
                Equal("previous\n– ☐ alpha\n– ☑ beta", Content(reopened).Text);
                Native(reopened).Document.Selection.SetRange(9, Native(reopened).Document.Selection.StoryLength - 1);
                Call(reopened, "SetList", "bullet");
                Equal("previous\n☐ alpha\n☑ beta", Content(reopened).Text);
                Require(Native(reopened).Document.GetRange(9, 14).ParagraphFormat.ListType == MarkerType.Bullet, "Bullet conversion failed.");
                Require(Native(reopened).Document.GetRange(0, 8).ParagraphFormat.ListType != MarkerType.Bullet, "List affected the preceding paragraph.");
                return "Dash markers convert to bullets while literal checkbox characters and the preceding paragraph stay intact.";
            });

            Check("empty-final-paragraph-list", () =>
            {
                var note = NewNote("");
                Call(note, "BeginEditing");
                Call(note, "SetList", "dash");
                Equal("–", Content(note).Text);
                return "A fresh empty note still accepts the retained dash-list marker.";
            });

            Check("hyperlink-and-format-roundtrip", () =>
            {
                var note = NewNote("example");
                Call(note, "BeginEditing");
                Native(note).Document.Selection.SetRange(0, 7);
                var range = (ITextRange)Call(note, "LinkRange")!;
                Require((bool)Call(note, "ApplyLink", range, "https://example.com/note", null)!, "Valid link was rejected.");
                var saved = Content(note);
                Equal("example", saved.Text);
                var reopened = NewNote(saved.Text, saved.Rtf);
                Equal("https://example.com/note", Native(reopened).Document.GetRange(0, 7).Link.Trim('\uFDDF', '"'));
                return "A friendly-name HTTP hyperlink survives reload without polluting the plain-text mirror.";
            });

            Check("link-form-and-selected-paragraph-preservation", () =>
            {
                const string text = "链接甲\n第二行内容";
                foreach (var selectionEnd in new[] { 3, 4 })
                {
                    var note = NewNote(text);
                    Call(note, "BeginEditing");
                    var doc = Native(note).Document;
                    doc.Selection.SetRange(0, selectionEnd);
                    var flyout = (Flyout)Call(note, "BuildLinkFlyout")!;
                    Require(flyout.AllowFocusOnInteraction, "Link form does not accept input focus.");
                    Equal(Microsoft.UI.Xaml.Controls.Primitives.FlyoutShowMode.Standard, flyout.ShowMode);
                    Require(Field<Dictionary<string, Button>>(note, "toggles")["link"].AllowFocusOnInteraction, "Link anchor disallows input focus.");
                    var fields = ((StackPanel)flyout.Content).Children.OfType<TextBox>().ToArray();
                    Equal(2, fields.Length);
                    Require(fields.All(field => !field.IsReadOnly && field.IsEnabled), "A link input is disabled or read-only.");
                    fields[0].Text = "https://example.com/card-audit";
                    var range = (ITextRange)Call(note, "LinkRange")!;
                    // Moving the native selection after opening must not redirect the saved range.
                    doc.Selection.SetRange(doc.Selection.StoryLength - 1, doc.Selection.StoryLength - 1);
                    Require((bool)Call(note, "ApplyLink", range, fields[0].Text, null)!, "First-line link was rejected.");
                    Equal(text, Content(note).Text);
                    var saved = Content(note);
                    var reopened = NewNote(saved.Text, saved.Rtf);
                    Equal(text, Content(reopened).Text);
                    Equal("https://example.com/card-audit", Native(reopened).Document.GetRange(0, 3).Link.Trim('\uFDDF', '"'));
                    var end = Native(reopened).Document.Selection.StoryLength - 1;
                    Equal("", Native(reopened).Document.GetRange(end - 2, end - 1).Link.Trim('\uFDDF', '"'));
                }
                var multi = NewNote(text);
                SelectAll(multi);
                Require((bool)Call(multi, "ApplyLink", Call(multi, "LinkRange"), "https://example.com/paragraphs", null)!, "Multi-paragraph link was rejected.");
                Equal(text, Content(multi).Text);
                var multiSaved = Content(multi);
                Equal(text, Content(NewNote(multiSaved.Text, multiSaved.Rtf)).Text);
                return "The standard link form has two editable fields and a focusable anchor; linking the first line with or without a selected CR preserves both paragraphs through save/reopen. Actual popup keyboard routing requires visible acceptance.";
            });

            Check("link-insertion-update-removal-and-invalid-input", () =>
            {
                var note = NewNote("before ");
                var doc = Native(note).Document;
                Call(note, "BeginEditing");
                doc.Selection.SetRange(7, 7);
                var range = (ITextRange)Call(note, "LinkRange")!;
                foreach (var invalid in new[] { "javascript:alert(1)", "file:///C:/test", "not a url", "https://example.com/\"bad" })
                {
                    var before = Content(note);
                    Require(!(bool)Call(note, "ApplyLink", range, invalid, "label")!, "Unsafe/invalid URL was accepted.");
                    Equal(before, Content(note));
                }
                Require((bool)Call(note, "ApplyLink", range, "https://example.com/insert", "链接正文")!, "Insertion without selection failed.");
                Equal("before 链接正文", Content(note).Text);
                Equal("https://example.com/insert", range.Link.Trim('\uFDDF', '"'));
                var end = range.EndPosition;
                doc.Selection.SetRange(end - 2, end - 2);
                var existing = (ITextRange)Call(note, "LinkRange")!;
                // ITextRange.Length is signed by its active end; a negative length
                // still represents a nonempty, correctly resolved hyperlink.
                Require(existing.EndPosition > existing.StartPosition, "Caret inside hyperlink did not resolve the existing link.");
                Require((bool)Call(note, "ApplyLink", existing, "mailto:test@example.com", null)!, "Existing link update failed.");
                Equal("before 链接正文", Content(note).Text);
                Call(note, "RemoveLink", existing);
                Equal("before 链接正文", Content(note).Text);
                Equal("", existing.Link.Trim('\uFDDF', '"'));
                doc.Selection.TypeText(" 继续正文");
                Equal("before 链接正文 继续正文", Content(note).Text);
                var urlOnly = NewNote("");
                Require((bool)Call(urlOnly, "ApplyLink", Call(urlOnly, "LinkRange"), "https://example.com/", null)!, "URL-only insertion failed.");
                Equal("https://example.com/", Content(urlOnly).Text);
                return "Invalid URLs leave the document unchanged; insertion, caret-based update/removal, default URL labels and subsequent native body typing preserve text.";
            });

            Check("rtf-mismatch-invalid-and-literal-escape", () =>
            {
                foreach (var rtf in new[] { "not-rtf", "{\\rtf1 different}", "{\\rtf1{\\pict 001122}}" })
                    Equal("authoritative", Content(NewNote("authoritative", rtf)).Text);
                var literal = NewNote("\\object {literal}");
                var saved = Content(literal);
                var reopened = NewNote(saved.Text, saved.Rtf);
                Equal(saved.Text, Content(reopened).Text);
                Require(Get<bool>(reopened, "CanSave"), "Literal escaped RTF control text was rejected.");
                return "Malformed, mismatched and embedded-picture RTF fall back to text; literal backslashes/braces remain saveable.";
            });

            Check("repeated-rtf-reopen-does-not-grow-trailing-paragraphs", () =>
            {
                var histories = new List<(string Input, List<int> Lengths, List<string> Stories, List<string> Rtfs)>();
                var nativeHistories = new List<object>();
                foreach (var mode in new[] { "document", "range", "defaults" })
                {
                    var native = new RichEditBox();
                    native.Document.SetText(TextSetOptions.None, "first\r\rthird\r\r");
                    var lengths = new List<int>();
                    var rtfs = new List<string>();
                    for (var i = 0; i < 6; i++)
                    {
                        lengths.Add(native.Document.Selection.StoryLength);
                        string rtf;
                        if (mode == "range") native.Document.GetRange(0, native.Document.Selection.StoryLength - 1).GetText(TextGetOptions.FormatRtf, out rtf);
                        else native.Document.GetText(TextGetOptions.FormatRtf, out rtf);
                        rtfs.Add(rtf.TrimEnd('\0'));
                        native = new RichEditBox();
                        native.Document.SetText(TextSetOptions.FormatRtf | (mode == "defaults" ? TextSetOptions.ApplyRtfDocumentDefaults : 0), rtf.TrimEnd('\0'));
                    }
                    nativeHistories.Add(new { mode, lengths, rtfs });
                }
                File.WriteAllText(Path.Combine(Path.GetDirectoryName(Program.ReportPath)!, "native-reopen-diagnostics.json"),
                    JsonSerializer.Serialize(nativeHistories, new JsonSerializerOptions { WriteIndented = true }));
                foreach (var input in new[] { "", "first\n\nthird", "first\nthird\n\n", "formatted\nnumber one\nnumber two\n☐ todo one\n\nlast\n\n" })
                {
                    var note = NewNote(input);
                    var rich = input.StartsWith("formatted", StringComparison.Ordinal);
                    if (rich)
                    {
                        var doc = Native(note).Document;
                        Call(note, "BeginEditing");
                        doc.Selection.SetRange(input.IndexOf("number one", StringComparison.Ordinal), input.IndexOf("number two", StringComparison.Ordinal) + "number two".Length);
                        Call(note, "SetList", "number");
                        doc.Selection.SetRange(0, "formatted".Length);
                        Call(note, "Character", (Action<ITextCharacterFormat>)(f =>
                        {
                            f.Bold = FormatEffect.On;
                            f.ForegroundColor = Color.FromArgb(255, 192, 44, 214);
                        }));
                        doc.Selection.Link = "\"https://example.com/reopen\"";
                    }
                    var expectedPlain = Content(note).Text;
                    var lengths = new List<int>();
                    var stories = new List<string>();
                    var rtfs = new List<string>();
                    for (var i = 0; i < 6; i++)
                    {
                        lengths.Add(Native(note).Document.Selection.StoryLength);
                        Native(note).Document.GetText(TextGetOptions.AllowFinalEop, out var story);
                        stories.Add(story);
                        var saved = Content(note);
                        Equal(expectedPlain, saved.Text);
                        if (rich)
                        {
                            var span = Native(note).Document.GetRange(0, "formatted".Length);
                            Require(span.CharacterFormat.Bold == FormatEffect.On, "Repeated RTF open lost bold.");
                            Equal(Color.FromArgb(255, 192, 44, 214), span.CharacterFormat.ForegroundColor);
                            Equal("https://example.com/reopen", span.Link.Trim('\uFDDF', '"'));
                            Require(saved.Text.Contains("☐ todo one", StringComparison.Ordinal), "Repeated RTF open changed literal text.");
                        }
                        rtfs.Add(saved.Rtf);
                        note = NewNote(saved.Text, saved.Rtf);
                    }
                    histories.Add((input, lengths, stories, rtfs));
                }
                File.WriteAllText(Path.Combine(Path.GetDirectoryName(Program.ReportPath)!, "reopen-diagnostics.json"),
                    JsonSerializer.Serialize(histories.Select(h => new { input = h.Input, lengths = h.Lengths, stories = h.Stories, rtfs = h.Rtfs }), new JsonSerializerOptions { WriteIndented = true }));
                foreach (var history in histories)
                {
                    Require(history.Lengths.Distinct().Count() == 1, "Native story grew across reopen: " + string.Join(",", history.Lengths));
                    Require(history.Stories.Distinct().Count() == 1, "Repeated RTF open changed the complete native story, including explicit empty paragraphs.");
                }
                return "Five successive native RTF reopen operations preserve the complete story, paragraph count, and supported formatting.";
            });

            Check("length-limits-preserve-unsaved-draft", () =>
            {
                var note = NewNote(new string('字', 4000));
                Require(Get<bool>(note, "CanSave"), "4000 Unicode scalar values must be valid.");
                Call(note, "BeginEditing");
                Native(note).Document.SetText(TextSetOptions.None, new string('字', 4001));
                Require(!Get<bool>(note, "CanSave"), "4001 Unicode scalar values must be rejected.");
                Call(note, "FinishEditing");
                Require(Get<bool>(note, "IsEditing") && !Native(note).IsReadOnly, "Invalid draft was made read-only.");
                Require(Content(note).Text.Length == 4001, "Over-limit draft was truncated.");
                Require(Field<TextBlock>(note, "error").Visibility == Visibility.Visible, "Length error is not visible.");
                Native(note).Document.SetText(TextSetOptions.None, string.Concat(Enumerable.Repeat("😀", 4000)));
                Require(Get<bool>(note, "CanSave"), "Surrogate pairs were incorrectly counted as two characters.");
                return "4000 scalars pass; 4001 stays editable with its complete draft; 4000 emoji pass.";
            });

            Check("synthetic-rich-text-fixture-export", () =>
            {
                const string text = "formatted\nparagraph one\nparagraph two\nnumber one\nnumber two\n☐ todo one\ndash one";
                var note = NewNote(text);
                Call(note, "BeginEditing");
                var doc = Native(note).Document;
                void Select(string first, string last) => doc.Selection.SetRange(text.IndexOf(first, StringComparison.Ordinal), text.IndexOf(last, StringComparison.Ordinal) + last.Length);
                Select("dash one", "dash one");
                Call(note, "SetList", "dash");
                Select("number one", "number two");
                Call(note, "SetList", "number");
                doc.Selection.SetRange(0, "formatted".Length);
                Call(note, "Character", (Action<ITextCharacterFormat>)(f =>
                {
                    f.Bold = FormatEffect.On;
                    f.ForegroundColor = Color.FromArgb(255, 192, 44, 214);
                }));
                doc.Selection.Link = "\"https://example.com/note\"";
                var saved = Content(note);
                var reopened = NewNote(saved.Text, saved.Rtf);
                Equal(saved.Text, Content(reopened).Text);
                Require(Get<bool>(reopened, "CanSave"), "Synthetic rich fixture is not saveable.");
                var path = Path.Combine(Path.GetDirectoryName(Program.ReportPath)!, "roundtrip-fixture.json");
                File.WriteAllText(path, JsonSerializer.Serialize(new { text = saved.Text, rtf = saved.Rtf }, new JsonSerializerOptions { WriteIndented = true }));
                return "Exported synthetic native RTF with bold, color, links, numbering, literal symbols, and dashes to roundtrip-fixture.json.";
            });

            Check("product-card-checkbox-template-measurement", () =>
            {
                var box = (CheckBox)Activator.CreateInstance(product.GetType("LiveListen.Controls.CardCheckBox", throwOnError: true)!)!;
                box.Style = (Style)Resources["DefaultCheckBoxStyle"];
                host.Children.Add(box);
                box.ApplyTemplate();
                box.Measure(new Size(100, 100));
                box.Arrange(new Rect(0, 0, 100, 32));
                box.UpdateLayout();
                var rectangle = Descendants(box).OfType<FrameworkElement>().FirstOrDefault(e => e.Name == "NormalRectangle");
                Require(rectangle is not null, "CheckBox template rectangle was unavailable in hidden-window layout; descendants="
                    + string.Join(",", Descendants(box).OfType<FrameworkElement>().Select(e => e.GetType().Name + ":" + e.Name)));
                Equal(14d, rectangle!.Width);
                Equal(14d, rectangle.Height);
                Require(box.MinHeight >= 28, "Checkbox hit area fell below 28 DIPs.");
                var glyph = Descendants(box).OfType<FrameworkElement>().FirstOrDefault(e => e.Name == "CheckGlyph");
                Require(glyph is not null, "Checkbox check glyph was not found.");
                Equal(14d, glyph!.Width);
                Equal(14d, glyph.Height);
                box.IsChecked = true;
                box.IsChecked = false;
                return "Real product CardCheckBox template has a 14×14-DIP rectangle/check glyph and >=28-DIP hit height; native checked state toggles.";
            });
        }
        catch (Exception e)
        {
            failed++;
            checks.Add(new { name = "setup", passed = false, detail = e.ToString() });
        }
        finally { Finish(); }
    }

    private object NewNote(string text, string? rtf = null)
    {
        var note = Activator.CreateInstance(editorType)!;
        var lookType = product.GetType("LiveListen.Services.SurfaceLook", true)!;
        var look = Activator.CreateInstance(lookType, [false, Color.FromArgb(255, 30, 30, 30), Color.FromArgb(255, 249, 249, 249), Color.FromArgb(30, 0, 0, 0), 8f, "none", 12d])!;
        Call(note, "Apply", look);
        host.Children.Add(Get<Grid>(note, "View"));
        Call(note, "Load", text, rtf);
        return note;
    }

    private static IEnumerable<DependencyObject> Descendants(DependencyObject root)
    {
        for (var i = 0; i < VisualTreeHelper.GetChildrenCount(root); i++)
        {
            var child = VisualTreeHelper.GetChild(root, i);
            yield return child;
            foreach (var nested in Descendants(child)) yield return nested;
        }
    }

    private static void SelectAll(object note) => Native(note).Document.Selection.SetRange(0, Native(note).Document.Selection.StoryLength - 1);
    private static RichEditBox Native(object note) => Field<RichEditBox>(note, "editor");
    private static (string Text, string Rtf) Content(object note) => ((string, string))Call(note, "GetContent")!;
    private static T Field<T>(object instance, string name) => (T)instance.GetType().GetField(name, BindingFlags.Instance | BindingFlags.NonPublic)!.GetValue(instance)!;
    private static T Get<T>(object instance, string name) => (T)instance.GetType().GetProperty(name)!.GetValue(instance)!;
    private static object? Call(object instance, string name, params object?[] arguments) => instance.GetType().GetMethod(name, BindingFlags.Instance | BindingFlags.Public | BindingFlags.NonPublic)!.Invoke(instance, arguments);
    private static void Require(bool value, string message) { if (!value) throw new InvalidOperationException(message); }
    private static void Equal<T>(T expected, T actual) => Require(EqualityComparer<T>.Default.Equals(expected, actual), $"Expected {JsonSerializer.Serialize(expected)}, got {JsonSerializer.Serialize(actual)}.");

    private void Finish()
    {
        if (finishing) return;
        finishing = true;
        var assemblyPath = Path.Combine(AppContext.BaseDirectory, "LiveListen.dll");
        var winUiPath = Path.Combine(AppContext.BaseDirectory, "Microsoft.UI.Xaml.dll");
        Directory.CreateDirectory(Path.GetDirectoryName(Program.ReportPath)!);
        File.WriteAllText(Program.ReportPath, JsonSerializer.Serialize(new
        {
            generatedAtUtc = DateTimeOffset.UtcNow,
            productAssemblySha256 = File.Exists(assemblyPath) ? Convert.ToHexString(SHA256.HashData(File.ReadAllBytes(assemblyPath))) : "",
            winUiNativeFileVersion = File.Exists(winUiPath) ? FileVersionInfo.GetVersionInfo(winUiPath).FileVersion : null,
            winUiNativeSha256 = File.Exists(winUiPath) ? Convert.ToHexString(SHA256.HashData(File.ReadAllBytes(winUiPath))) : null,
            hiddenWindow = true,
            activatedWindow = false,
            synthesizedInput = false,
            failed,
            checks,
            limits = new[] { "No mouse/keyboard automation or foreground activation.", "Native checklist visual size, popup placement, cursor, pointer hover and physical click behavior require separate visible UI acceptance." },
        }, new JsonSerializerOptions { WriteIndented = true }));
        Program.ResultCode = failed == 0 ? 0 : 1;
        window?.Close();
        Exit();
    }
}
