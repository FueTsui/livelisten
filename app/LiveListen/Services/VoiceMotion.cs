namespace LiveListen.Services;

/// <summary>Presentation-only envelope. Audio comes exclusively from the engine's level event.</summary>
internal sealed class VoiceMotion
{
    private double target;
    private double lastInput = double.NegativeInfinity;

    public string Phase { get; private set; } = "idle";
    public bool Visible { get; private set; }
    public bool Animations { get; set; } = true;
    public bool Running => Visible && Animations;
    public bool Processing => Phase is "connecting" or "finishing";
    public double Level { get; private set; }
    public double Time { get; private set; }
    public int IntervalMs => Phase == "listening" || Processing ? 34 : 67;

    public void SetPhase(string phase)
    {
        if (Phase == phase) return;
        Phase = phase;
        ResetLevel();
    }

    public void SetVisible(bool visible)
    {
        if (Visible == visible) return;
        Visible = visible;
        ResetLevel();
    }

    public void OnLevel(double value, double now)
    {
        if (!Visible || Phase != "listening") return;
        target = double.IsFinite(value) ? Math.Clamp(value * 1.6, 0, 1) : 0;
        lastInput = now;
    }

    public void Step(double elapsed, double now)
    {
        if (!Running) return;
        var dt = Math.Clamp(elapsed, 0, 0.1);
        Time += dt;
        // A missing event must not leave the last loud frame stuck on screen.
        var next = Phase == "listening" && now - lastInput <= 0.25 ? target : 0;
        Level += (next - Level) * (1 - Math.Exp(-dt * (next > Level ? 20 : 10)));
        if (Level < 0.0001) Level = 0;
    }

    private void ResetLevel()
    {
        target = Level = 0;
        lastInput = double.NegativeInfinity;
    }
}
