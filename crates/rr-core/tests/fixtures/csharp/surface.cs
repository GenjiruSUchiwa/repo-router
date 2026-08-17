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
}

public enum Mode
{
    Fast,
    Slow
}

namespace App.Legacy
{
    public class LegacyGate
    {
        public void Serve() {}
    }
}
