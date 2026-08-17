#include <stdio.h>
#include "local.h"

typedef struct {
    int count;
} Server;

typedef void (*Handler)(int);

enum Mode { FAST, SLOW };

static int hidden(void) {
    return 0;
}

int run(Server *server) {
    return helper(server);
}
