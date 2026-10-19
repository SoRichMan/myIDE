#include <cstdio>
int helper(int x) { return x * 2; }
struct Foo { int a; double b; char c[16]; }; struct Big { int f[100]; };
int main() {
    Foo foo = {42, 3.14, "hello"}; Big big = {};
    int i = 0, sum = 0; while (i < 10) {
        ++i;
        sum += i;
    }
    int h = helper(sum);
    if (h > 0) {
        printf("checkpoint ok\n");
    }
    printf("sum=%d foo.a=%d\n", sum, foo.a);
    return 0;
}
