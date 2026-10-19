#include <cstdio>

#include "math_utils.h"

int main() {
    int sum = add(3, 4);
    double sq = square(2.5);
    Point origin{0.0, 0.0};
    Point target{3.0, 4.0};
    std::printf("sum=%d sq=%.2f manhattan=%.2f\n", sum, sq, manhattan(origin, target));
    return static_cast<int>(sq);
}
