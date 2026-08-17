package app;

public class Service implements Runner {
    private int secret;
    int packageField;
    protected int inherited;
    public static final int MAX = 3;

    public Service(String name) {}

    public void run() {
        helper();
    }

    private void hidden() {}

    void packageRun() {}

    @Test
    public void aTest() {}
}

interface Runner {
    void run();
}

enum Mode {
    FAST,
    SLOW
}

public record Point(int x, int y) {}
