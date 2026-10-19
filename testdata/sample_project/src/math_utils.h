#pragma once

/// 两个整数相加。
/// @param a 第一个加数
/// @param b 第二个加数
/// @return a + b
int add(int a, int b);

/// 计算平方。
/// @param x 输入数值
/// @return x * x
double square(double x);

/// 二维点。
struct Point {
    double x;
    double y;
};

/// 两点间的曼哈顿距离。
double manhattan(const Point& p, const Point& q);
