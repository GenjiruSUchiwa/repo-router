#include <string>
#include "local.h"

namespace app {
class Service {
public:
    Service();
    void run();

private:
    int secret;
};

void Service::run() {}

struct Point {
    int x;
    int y;
};

template <typename T>
T identity(T value) {
    return value;
}
}
