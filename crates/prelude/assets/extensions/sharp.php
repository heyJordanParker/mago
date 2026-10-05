<?php

namespace Sharp;

final class Int
{
    /**
     * Parses a string that holds only an int, as C#'s `int.Parse` does: `[ws][sign]digits[ws]`.
     *
     * @throws \ValueError when `$value` is not a string or does not hold an int.
     * @throws \ArithmeticError when the int is outside PHP_INT_MIN to PHP_INT_MAX.
     */
    public static function parse(mixed $value): int {}

    /**
     * Gives null where `parse` throws.
     *
     * @pure
     */
    public static function tryParse(mixed $value): ?int {}
}

final class Float
{
    /**
     * Parses a string that holds only a float: `[ws][sign](digits[.digits] | .digits)([eE][sign]digits)?[ws]`.
     *
     * @throws \ValueError when `$value` is not a string or does not hold a float.
     * @throws \ArithmeticError when the float is outside -PHP_FLOAT_MAX to PHP_FLOAT_MAX.
     */
    public static function parse(mixed $value): float {}

    /**
     * Gives null where `parse` throws.
     *
     * @pure
     */
    public static function tryParse(mixed $value): ?float {}
}
