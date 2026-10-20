<?php

use JetBrains\PhpStorm\Deprecated;
use JetBrains\PhpStorm\Immutable;
use JetBrains\PhpStorm\Internal\LanguageLevelTypeAware;
use JetBrains\PhpStorm\Internal\PhpStormStubsElementAvailable;
use JetBrains\PhpStorm\Internal\TentativeType;
use JetBrains\PhpStorm\Pure;

/**
 * The <b>ReflectionParameter</b> class retrieves
 * information about function's or method's parameters.
 *
 * @link https://php.net/manual/en/class.reflectionparameter.php
 */
class ReflectionParameter implements Reflector
{
    /**
     * @var string Name of the parameter, same as calling the {@see ReflectionParameter::getName()} method
     */
    #[LanguageLevelTypeAware(['8.1' => 'string'], default: '')]
    public $name;

    /**
     * Construct
     *
     * @link https://php.net/manual/en/reflectionparameter.construct.php
     * @param callable $function The function to reflect parameters from.
     * @param string|int $param Either an integer specifying the position
     * of the parameter (starting with zero), or a the parameter name as string.
     * @throws ReflectionException if the function or parameter does not exist.
     */
    public function __construct($function, #[LanguageLevelTypeAware(['8.0' => 'string|int'], default: '')] $param) {}

    /**
     * Exports
     *
     * @link https://php.net/manual/en/reflectionparameter.export.php
     * @param string $function The function name.
     * @param string $parameter The parameter name.
     * @param bool $return Setting to {@see true} will return the export,
     * as opposed to emitting it. Setting to {@see false} (the default) will do the
     * opposite.
     * @return string|null The exported reflection.
     * @removed 8.0
     */
    public static function export($function, $parameter, $return = false) {}

    /**
     * Returns the string representation of the ReflectionParameter object.
     *
     * @link https://php.net/manual/en/reflectionparameter.tostring.php
     * @return string The string.
     */
    #[LanguageLevelTypeAware(['7.0' => 'string'], default: '')]
    public function __toString() {}

    /**
     * Gets parameter name
     *
     * @link https://php.net/manual/en/reflectionparameter.getname.php
     * @return string The name of the reflected parameter.
     */
    public function getName(): string {}

    /**
     * Checks if passed by reference
     *
     * @link https://php.net/manual/en/reflectionparameter.ispassedbyreference.php
     * @return bool {@see true} if the parameter is passed in by reference, otherwise {@see false}
     */
    public function isPassedByReference(): bool {}

    /**
     * Returns whether this parameter can be passed by value
     *
     * @link https://php.net/manual/en/reflectionparameter.canbepassedbyvalue.php
     * @return bool {@see true} if the parameter can be passed by value, {@see false} otherwise.
     * Prior to PHP 8.1, {@see null} was returned in case of an error.
     * @since 5.4
     */
    public function canBePassedByValue(): bool {}

    /**
     * Gets declaring function
     *
     * @link https://php.net/manual/en/reflectionparameter.getdeclaringfunction.php
     * @return ReflectionFunctionAbstract A {@see ReflectionFunctionAbstract} object.
     * @since 5.2
     */
    public function getDeclaringFunction(): ReflectionFunctionAbstract {}

    /**
     * Gets declaring class
     *
     * @link https://php.net/manual/en/reflectionparameter.getdeclaringclass.php
     * @return ReflectionClass|null A {@see ReflectionClass} object or {@see null} if
     * called on function.
     */
    public function getDeclaringClass(): ?ReflectionClass {}

    /**
     * Gets the class type hinted for the parameter as a ReflectionClass object.
     *
     * @link https://php.net/manual/en/reflectionparameter.getclass.php
     * @return ReflectionClass|null A {@see ReflectionClass} object.
     * @see ReflectionParameter::getType()
     */
    public function getClass(): ?ReflectionClass {}

    /**
     * Checks if the parameter has a type associated with it.
     *
     * @link https://php.net/manual/en/reflectionparameter.hastype.php
     * @return bool {@see true} if a type is specified, {@see false} otherwise.
     * @since 7.0
     */
    public function hasType(): bool {}

    /**
     * Gets a parameter's type
     *
     * @link https://php.net/manual/en/reflectionparameter.gettype.php
     * @return ReflectionType|null Returns a {@see ReflectionType} object if a
     * parameter type is specified, {@see null} otherwise.
     * @since 7.0
     */
    #[LanguageLevelTypeAware(
        [
            '7.1' => 'ReflectionNamedType|null',
            '8.0' => 'ReflectionNamedType|ReflectionUnionType|null',
            '8.1' => 'ReflectionNamedType|ReflectionUnionType|ReflectionIntersectionType|null'
        ],
        default: 'ReflectionType|null'
    )]
    public function getType(): ?ReflectionType {}

    /**
     * Checks if parameter expects an array
     *
     * @link https://php.net/manual/en/reflectionparameter.isarray.php
     * @return bool {@see true} if an array is expected, {@see false} otherwise.
     * @see ReflectionParameter::getType()
     */
    public function isArray(): bool {}

    /**
     * Returns whether parameter MUST be callable
     *
     * @link https://php.net/manual/en/reflectionparameter.iscallable.php
     * @return bool Returns {@see true} if the parameter is callable, {@see false} if it is not.
     * Prior to PHP 8.1, {@see null} was returned on failure.
     * @since 5.4
     * @see ReflectionParameter::getType()
     */
    public function isCallable(): bool {}

    /**
     * Checks if null is allowed
     *
     * @link https://php.net/manual/en/reflectionparameter.allowsnull.php
     * @return bool Returns {@see true} if {@see null} is allowed,
     * otherwise {@see false}
     */
    public function allowsNull(): bool {}

    /**
     * Gets parameter position
     *
     * @link https://php.net/manual/en/reflectionparameter.getposition.php
     * @return int The position of the parameter, left to right, starting at position #0.
     * @since 5.2
     */
    public function getPosition(): int {}

    /**
     * Checks if optional
     *
     * @link https://php.net/manual/en/reflectionparameter.isoptional.php
     * @return bool Returns {@see true} if the parameter is optional, otherwise {@see false}
     * @since 5.0
     */
    public function isOptional(): bool {}

    /**
     * Checks if a default value is available
     *
     * @link https://php.net/manual/en/reflectionparameter.isdefaultvalueavailable.php
     * @return bool Returns {@see true} if a default value is available, otherwise {@see false}
     * @since 5.0
     */
    public function isDefaultValueAvailable(): bool {}

    /**
     * Gets default parameter value
     *
     * @link https://php.net/manual/en/reflectionparameter.getdefaultvalue.php
     * @return mixed The parameters default value.
     * @throws ReflectionException if the parameter is not optional
     * @since 5.0
     */
    public function getDefaultValue(): mixed {}

    /**
     * Returns whether the default value of this parameter is constant
     *
     * @link https://php.net/manual/en/reflectionparameter.isdefaultvalueconstant.php
     * @return bool Returns {@see true} if the default value is constant, and {@see false} otherwise.
     * @since 5.4
     */
    public function isDefaultValueConstant(): bool {}

    /**
     * Returns the default value's constant name if default value is constant or null
     *
     * @link https://php.net/manual/en/reflectionparameter.getdefaultvalueconstantname.php
     * @return string|null Returns string on success or {@see null} on failure.
     * @throws ReflectionException if the parameter is not optional
     * @since 5.4
     */
    public function getDefaultValueConstantName(): ?string {}

    /**
     * Returns whether this function is variadic
     *
     * @link https://php.net/manual/en/reflectionparameter.isvariadic.php
     * @return bool Returns {@see true} if the function is variadic, otherwise {@see false}
     * @since 5.6
     */
    public function isVariadic(): bool {}

    /**
     * Returns information about whether the parameter is a promoted.
     *
     * @link https://php.net/manual/en/reflectionparameter.ispromoted.php
     * @return bool Returns {@see true} if the parameter promoted or {@see false} instead
     * @since 8.0
     */
    public function isPromoted(): bool {}

    /**
     * Gets Attributes
     *
     * Returns all attributes declared on this parameter as an array of ReflectionAttribute.
     *
     * @link https://php.net/manual/en/reflectionparameter.getattributes.php
     * @template T
     *
     * Returns an array of parameter attributes.
     *
     * @param class-string<T>|null $name Name of an attribute class
     * @param int $flags Сriteria by which the attribute is searched.
     * @return ReflectionAttribute<T>[] Array of attributes, as a ReflectionAttribute object.
     * @since 8.0
     */
    public function getAttributes(?string $name = null, int $flags = 0): array {}

    /**
     * Clone
     *
     * @link https://php.net/manual/en/reflectionparameter.clone.php
     * @return void
     */
    final private function __clone(): void {}

    /**
     * Clone
     *
     * @link https://php.net/manual/en/reflectionparameter.clone.php
     * @return void
     */
    private function __clone(): void {}

    /**
     * @return string|false
     * @since 8.6
     */
    public function getDocComment(): string|false {}
}
