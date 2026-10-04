<?php

declare(strict_types=1);

namespace Mago\Sdk\Syntax;

/**
 * What a bare PHP# name refers to, as Mago's binder decided from its file alone. A PHP name has none.
 *
 * @api
 */
enum Binding: string
{
    /** A parameter, or a local declared with `let` or `const`. */
    case Local = 'Local';
    /** `this`, the object the method runs on. */
    case This = 'This';
    /** A class, written before `.`, such as `Calc` in the static call `Calc.make()`. */
    case ClassName = 'ClassName';
    /** A constant. */
    case Constant = 'Constant';
    /** A member of the enclosing class, written without `this.`. */
    case Member = 'Member';
}
