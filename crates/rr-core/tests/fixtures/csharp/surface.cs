using System;
using System.Collections.Generic;
using IO = System.IO;
using System.Linq;

namespace App.Core;

public delegate int Converter(string input);

public class Service : IService
{
    private int secret;
    internal int unitField;
    protected int inherited;
    public static readonly int Max = 3;
    public event EventHandler Changed;

    public Service(string name)
    {
        helper();
    }

    public int Count { get; set; }

    public void Run()
    {
        var order = new Order();
        order.Ship();
        int total = Compute();
        int Add(int a, int b) => a + b;
    }

    private void helper() {}

    [Fact]
    public void ATest() {}
}

public struct Point
{
    public int X;
}

public record Model(int Id, string Name);

public record struct Pair(int Left, int Right);

public interface IService
{
    void Run();
    int Size { get; }
    private void Trace() {}
}

public enum Mode
{
    Fast,
    Slow
}

public class Repo : System.Collections.Generic.List<int>
{
    public void Load()
    {
        var buffer = new IO.MemoryStream();
        var page = Fetch<int>();
        this.Emit<int>(page);
    }
}

public class Box : Repo
{
    [Obsolete("keep until 2 (the next major")] public void Drop() {}
}

namespace App.Legacy
{
    public class LegacyGate
    {
        public void Serve() {}
    }
}

class Hidden
{
    void Tick() {}

    class Inner
    {
        void Beat() {}
    }
}
