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

/**
 * The methods of a PHP# `List<T>`, as spec section 12 writes them. The analyzer checks a call on a list against
 * them, and the engine runs them on `Sharp\Collection`. A method without `@mutation-free` changes the list.
 *
 * @template T
 */
final class ListMethods
{
    /**
     * Appends `$value` after the last element.
     *
     * @param T $value
     */
    public function add(mixed $value): void {}

    /**
     * Replaces the element at `$index`.
     *
     * @param T $value
     *
     * @throws \OutOfRangeException when `$index` is not an index of the list.
     */
    public function set(int $index, mixed $value): void {}

    /**
     * @return T|null the element at `$index`, or null when `$index` is not an index of the list.
     *
     * @mutation-free
     */
    public function get(int $index): mixed {}

    /**
     * @return array<int, T> each index with its element, for `for (const [i, x] of list.entries())`.
     *
     * @mutation-free
     */
    public function entries(): array {}
}

/**
 * The methods of a PHP# `Map<K, V>`, as spec section 12 writes them. The analyzer checks a call on a map against
 * them, and the engine runs them on `Sharp\Collection`. A method without `@mutation-free` changes the map.
 *
 * @template K of array-key
 * @template V
 */
final class MapMethods
{
    /**
     * Removes the entry of `$key`, if the map has one.
     *
     * @param K $key
     */
    public function delete(int|string $key): void {}

    /**
     * @param K $key
     *
     * @return V|null the value of `$key`, or null when the map has no entry for it.
     *
     * @mutation-free
     */
    public function get(int|string $key): mixed {}
}
