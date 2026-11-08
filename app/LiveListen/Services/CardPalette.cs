using System.Text.Json.Nodes;

namespace LiveListen.Services;

/// <summary>A color-only preset that preserves the card's other appearance settings.</summary>
internal sealed record CardColorPreset(string Key, string Background, string Text, string Border)
{
    public JsonObject Apply(JsonObject baseStyle)
    {
        var style = (JsonObject)baseStyle.DeepClone();
        style["background"] = Background;
        style["text"] = Text;
        style["border"] = Border;
        return style;
    }

    public bool Matches(JsonObject? ownStyle) => ownStyle is not null
        && Theme.Parse(ownStyle["background"].Str()) == Theme.Parse(Background)
        && Theme.Parse(ownStyle["text"].Str()) == Theme.Parse(Text)
        && Theme.Parse(ownStyle["border"].Str()) == Theme.Parse(Border);
}

internal static class CardPalette
{
    public static IReadOnlyList<CardColorPreset> All { get; } = Array.AsReadOnly<CardColorPreset>(
    [
        new("white", "#FFFFFDFB", "", "#FFE2DDD6"),
        new("yellow", "#FFFFF6C9", "", "#FFE7D99B"),
        new("purple", "#FFF0E8FB", "", "#FFD8C8EC"),
        new("pink", "#FFFBE8EE", "", "#FFECCAD5"),
        new("mint", "#FFE4F4EC", "", "#FFC1DFD0"),
        new("blue", "#FFE5F0FC", "", "#FFC3D8EF"),
    ]);
}
