export interface DataTableSortItem {
  key: string;
  order?: boolean | 'asc' | 'desc';
}

export interface ServerTableOptions {
  page: number;
  itemsPerPage: number;
  sortBy: DataTableSortItem[];
  groupBy?: DataTableSortItem[];
}

export function createServerTableOptions(
  itemsPerPage: number,
  sortBy: DataTableSortItem[] = [],
  groupBy: DataTableSortItem[] = [],
): ServerTableOptions {
  return {page: 1, itemsPerPage, sortBy, groupBy}
}

export function toPageRequest(options: ServerTableOptions): PageRequest {
  return {
    page: options.page - 1,
    size: options.itemsPerPage,
    sort: options.sortBy.map(({key, order}) =>
      `${key},${order === true || order === 'desc' ? 'desc' : 'asc'}`,
    ),
  }
}
