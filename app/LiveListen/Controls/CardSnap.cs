using Windows.Graphics;

namespace LiveListen.Controls;

/// <summary>
/// Where a dragged desktop card settles: against the screen edges, beside, above or below
/// another card (lined up with it), or stacked on another card with a small offset so the
/// date of the card below stays visible. All rectangles are drawn cards in screen pixels.
/// </summary>
internal static class CardSnap
{
    /// <summary>How close an edge must come before it snaps, in DIPs.</summary>
    private const double ReachDip = 12;

    /// <summary>Space between snapped cards, in DIPs.</summary>
    private const double GapDip = 4;

    /// <summary>Offset of a card stacked on another, in DIPs.</summary>
    private const double StackXDip = 12;
    private const double StackYDip = 34;

    /// <param name="card">The dragged card where the pointer would put it.</param>
    /// <param name="work">Work area of the screen it is on.</param>
    /// <param name="others">The other cards on the desktop.</param>
    /// <param name="scale">Pixels per DIP.</param>
    /// <param name="edgeDip">Space to keep from the screen edges, in DIPs.</param>
    /// <returns>How far to move the card.</returns>
    public static PointInt32 Offset(RectInt32 card, RectInt32 work, IReadOnlyList<RectInt32> others, double scale, double edgeDip)
    {
        int Px(double dip) => (int)Math.Round(dip * scale);
        var reach = Px(ReachDip);
        var gap = Px(GapDip);
        var edge = Px(Math.Max(GapDip, edgeDip));

        // Dropped near the stacking spot of another card: settle there.
        var stackX = Px(StackXDip);
        var stackY = Px(StackYDip);
        foreach (var o in others)
        {
            var dx = o.X + stackX - card.X;
            var dy = o.Y + stackY - card.Y;
            if (Math.Abs(dx) <= stackX && Math.Abs(dy) <= reach)
            {
                return new PointInt32(dx, dy);
            }
        }

        var xs = new List<int> { work.X + edge - card.X, Right(work) - edge - Right(card) };
        var ys = new List<int> { work.Y + edge - card.Y, Bottom(work) - edge - Bottom(card) };
        foreach (var o in others)
        {
            var sameRow = card.Y < Bottom(o) + reach && Bottom(card) > o.Y - reach;
            var sameColumn = card.X < Right(o) + reach && Right(card) > o.X - reach;
            var hGap = Math.Max(o.X - Right(card), card.X - Right(o));
            var vGap = Math.Max(o.Y - Bottom(card), card.Y - Bottom(o));
            if (sameRow)
            {
                // Beside it.
                xs.Add(Right(o) + gap - card.X);
                xs.Add(o.X - gap - Right(card));
                if (hGap >= 0 && hGap <= gap + reach)
                {
                    // Tops or bottoms in line.
                    ys.Add(o.Y - card.Y);
                    ys.Add(Bottom(o) - Bottom(card));
                }
            }
            if (sameColumn)
            {
                // Above or below it.
                ys.Add(Bottom(o) + gap - card.Y);
                ys.Add(o.Y - gap - Bottom(card));
                if (vGap >= 0 && vGap <= gap + reach)
                {
                    // Left or right edges in line.
                    xs.Add(o.X - card.X);
                    xs.Add(Right(o) - Right(card));
                }
            }
        }
        return new PointInt32(Nearest(xs, reach), Nearest(ys, reach));
    }

    private static int Nearest(List<int> moves, int reach)
    {
        var best = 0;
        var found = false;
        foreach (var m in moves)
        {
            if (Math.Abs(m) <= reach && (!found || Math.Abs(m) < Math.Abs(best)))
            {
                best = m;
                found = true;
            }
        }
        return best;
    }

    private static int Right(RectInt32 r) => r.X + r.Width;

    private static int Bottom(RectInt32 r) => r.Y + r.Height;
}
