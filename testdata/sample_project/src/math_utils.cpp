#include "math_utils.h"

int add(int a, int b) {
    return a + b;
}

double square(double x) {
    return x * x;
}

double manhattan(const Point& p, const Point& q) {
    double dx = p.x > q.x ? p.x - q.x : q.x - p.x;
    double dy = p.y > q.y ? p.y - q.y : q.y - p.y;
    return dx + dy;
}
