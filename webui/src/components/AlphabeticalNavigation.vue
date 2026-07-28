<template>
  <div>
    <v-tooltip
      v-for="symbol in symbols"
      :key="symbol"
      :disabled="groupCount === undefined" location="top"
    >
      <template v-slot:activator="{ props }">
        <v-btn variant="text" size="small"
          icon
          @click="clicked(symbol)"
          :color="selected === symbol ? 'secondary' : undefined"
          :disabled="groupCount && selected !== symbol ? getCount(symbol) === 0 : false"
          v-bind="props"
        >
          {{ symbol }}
        </v-btn>
      </template>
      {{ getCount(symbol) }}
    </v-tooltip>

  </div>
</template>

<script lang="ts">
import {defineComponent, PropType} from 'vue'
import {GroupCountDto} from '@/types/komga-series'

export default defineComponent({
  name: 'AlphabeticalNavigation',
  data: function () {
    return {}
  },
  props: {
    groupCount: {
      type: Array as PropType<GroupCountDto[]>,
      required: false,
    },
    symbols: {
      type: Array,
      default: () => ['A', 'B', 'C', 'D', 'E', 'F', 'G', 'H', 'I', 'J', 'K', 'L', 'M', 'N', 'O', 'P', 'Q', 'R', 'S', 'T', 'U', 'V', 'W', 'X', 'Y', 'Z'],
    },
    selected: {
      type: String,
      required: false,
    },
  },
  methods: {
    clicked(symbol: string) {
      this.$emit('clicked', symbol)
    },
    getCount(symbol: string): number | undefined {
      if(!this.groupCount) return undefined
      const found = this.groupCount.find(g => g.group.toLowerCase() === symbol.toLowerCase())
      return found ? found.count : 0
    },
  },
})
</script>
