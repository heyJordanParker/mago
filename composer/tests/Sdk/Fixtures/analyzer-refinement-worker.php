<?php

declare(strict_types=1);

namespace Mago\Tests\Sdk\Fixtures;

use LogicException;
use Mago\Sdk\Analyzer\CodebaseScanContext;
use Mago\Sdk\Analyzer\CodebaseScanHook;
use Mago\Sdk\Analyzer\Declaration\ClassLikeRefinement;
use Mago\Sdk\Analyzer\Declaration\RefinedType;
use Mago\Sdk\Analyzer\Declaration\RefinementIssue;
use Mago\Sdk\Analyzer\Declaration\SignatureRefinement;
use Mago\Sdk\Analyzer\Declaration\TypeParameter;
use Mago\Sdk\Analyzer\Plugin;
use Mago\Sdk\Analyzer\PluginDefinition;
use Mago\Sdk\Analyzer\PluginRegistry;
use Mago\Sdk\Analyzer\Type;
use Mago\Sdk\Analyzer\Type\GenericParameterType;
use Mago\Sdk\Analyzer\Type\GenericParent;
use Mago\Sdk\Analyzer\Type\GenericParentKind;
use Mago\Sdk\Analyzer\Type\ReferenceType;
use Mago\Sdk\Analyzer\Type\ReferenceTypeKind;
use Mago\Sdk\Extension;
use Mago\Sdk\Reporting\Level;
use Mago\Sdk\Span;
use Mago\Sdk\Syntax\SourceFile;
use Mago\Sdk\Worker;

use function dirname;
use function strlen;
use function strpos;

require_once dirname(__DIR__, 4) . '/vendor/autoload.php';

/**
 * Refines declarations the way a framework adapter would: generic parameters on a class, and
 * applications of that class on another class's members, all read from the scanned source.
 *
 * @mago-expect lint:file-name
 */
final class DeclarationRefinementProofPlugin implements CodebaseScanHook, Plugin
{
    private const MARKER = '// refined';

    public function getTargets(): array
    {
        return ['src/**/*.php'];
    }

    public function getDefinition(): PluginDefinition
    {
        return new PluginDefinition(
            'declaration-refinement-proof',
            'Declaration refinement proof',
            'Refines declarations from codebase scans.',
        );
    }

    public function register(PluginRegistry $registry): void
    {
        $registry->registerCodebaseScanHook($this);
    }

    public function scan(CodebaseScanContext $context): void
    {
        foreach ($context->files as $file) {
            $marker = strpos($file->contents, self::MARKER);
            if ($marker === false) {
                continue;
            }

            $span = new Span($marker, $marker + strlen(self::MARKER));
            $context->refine(match ($file->path) {
                'src/Box.php' => self::box($file, $span),
                'src/Holder.php' => self::holder($file, $span),
                default => throw new LogicException("Unexpected refined file `{$file->path}`."),
            });
        }
    }

    private static function box(SourceFile $file, Span $span): ClassLikeRefinement
    {
        $item = Type::fromAtomic(
            new GenericParameterType(
                'T',
                self::symbol('Proof\Spec'),
                new GenericParent(GenericParentKind::ClassLike, 'proof\box'),
                null,
            ),
        );

        return new ClassLikeRefinement(
            $file->path,
            'Proof\Box',
            typeParameters: [new TypeParameter('T', new RefinedType(self::symbol('Proof\Spec'), $span))],
            properties: ['item' => new RefinedType($item, $span)],
            methods: ['get' => new SignatureRefinement(return: new RefinedType($item, $span))],
        );
    }

    private static function holder(SourceFile $file, Span $span): ClassLikeRefinement
    {
        return new ClassLikeRefinement(
            $file->path,
            'Proof\Holder',
            properties: [
                'excess' => new RefinedType(
                    self::symbol('Proof\Box', self::symbol('Proof\Spec'), self::symbol('Proof\Spec')),
                    $span,
                ),
                'outside' => new RefinedType(self::symbol('Proof\Box', self::symbol('Proof\Other')), $span),
            ],
            methods: [
                'wrong' => new SignatureRefinement(parameters: [
                    'box' => new RefinedType(self::symbol('Proof\Box', self::symbol('Proof\Spec')), $span),
                ]),
                'broad' => new SignatureRefinement(
                    parameters: ['box' => new RefinedType(
                        self::symbol('Proof\Box', self::symbol('Proof\Spec')),
                        $span,
                    )],
                    return: new RefinedType(self::symbol('Proof\Box', self::symbol('Proof\Special')), $span),
                ),
                'subtype' => new SignatureRefinement(return: new RefinedType(self::symbol('Proof\Spec'), $span)),
            ],
            issues: [new RefinementIssue(Level::Warning, 'marker', 'This declaration is refined.', $span)],
        );
    }

    private static function symbol(string $class, Type ...$arguments): Type
    {
        return Type::fromAtomic(
            new ReferenceType(
                ReferenceTypeKind::Symbol,
                $class,
                $arguments === [] ? null : [...$arguments],
                null,
                null,
                null,
                null,
            ),
        );
    }
}

(new Worker(new Extension(
    identifier: 'mago/declaration-refinement-proof',
    name: 'Mago declaration refinement proof',
    version: '1.0.0',
    analyzerPlugins: [new DeclarationRefinementProofPlugin()],
)))->run();
