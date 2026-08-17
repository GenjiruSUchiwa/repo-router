import Foundation

public class Service {
    public var name: String
    package var group: String
    private var secret: Int

    public init(name: String) {
        self.name = name
        self.group = "x"
        self.secret = 1
    }

    public func run() {
        helper(name)
    }
}

protocol Runner {
    func run()
}

enum Mode {
    case fast, slow
}

struct Point {
    var x: Int
    var y: Int
}

public func bare(_ value: Int) -> Int {
    return value
}
