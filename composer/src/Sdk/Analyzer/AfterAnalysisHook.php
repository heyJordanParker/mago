<?php

declare(strict_types=1);

namespace Mago\Sdk\Analyzer;

/**
 * Runs once after all file results have been merged.
 *
 * A worker serves many analyses. Each analysis runs on a fresh `clone` of the hook as it was
 * registered, before any analysis, so scalar and array properties start as registered every time.
 * A hook that keeps state in an object property deep-copies that object in `__clone`.
 *
 * @api
 */
interface AfterAnalysisHook
{
    public function afterAnalysis(AfterAnalysisContext $context): void;
}
